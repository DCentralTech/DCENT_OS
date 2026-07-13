// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) D-Central Technologies — https://d-central.tech

//! Fail-closed ownership for the Linux hardware watchdog.
//!
//! A watchdog file descriptor is a safety resource, not a background timer.
//! This module owns its worker thread, requires an observed arm admission
//! before an engine may energize hardware, and makes magic-close reachable
//! only from an admitted teardown carrying engine-issued shutdown evidence.
//! Dropping the owner, losing the command channel, missing a deadline, or
//! suppressing feeds never writes the magic-close byte.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::oneshot;
use tracing::{error, info, warn};

use dcentrald_hal::watchdog::Watchdog;

use crate::config::WatchdogConfig;
use crate::runtime::thread_guard::{join_thread_bounded, ThreadStopOutcome, ThreadStopSummary};

const WATCHDOG_ADMISSION_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const DEFAULT_WATCHDOG_STOP_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const DEFAULT_WATCHDOG_TEARDOWN_GRACE: Duration = Duration::from_secs(30);

/// The kicker period must remain non-zero even if validation was bypassed.
pub(crate) fn watchdog_interval_secs(kick_interval_s: u64) -> u64 {
    kick_interval_s.max(1)
}

pub(crate) fn watchdog_teardown_kick_allowed(deadline: Instant, now: Instant) -> bool {
    now < deadline
}

pub(crate) fn watchdog_stall_limit(
    effective_timeout_s: u64,
    kick_secs: u64,
    expected_liveness_interval: Option<Duration>,
) -> u64 {
    let kick_secs = kick_secs.max(1);
    let half_window_limit = ((effective_timeout_s / 2) / kick_secs).max(2);
    let cadence_limit = expected_liveness_interval
        .map(|interval| {
            ((interval.as_secs_f64() / kick_secs as f64).ceil() as u64).saturating_add(2)
        })
        .unwrap_or(0);
    half_window_limit.max(cadence_limit)
}

/// Pure mining-liveness decision. The caller must latch the first `false`
/// result terminally; a late counter advance must never cancel a reset that
/// safety policy has already requested.
pub(crate) fn watchdog_kick_decision(
    current: u64,
    last_live: u64,
    stalls: u64,
    stall_limit: u64,
) -> (bool, u64, u64) {
    if current == last_live {
        let stalls = stalls.saturating_add(1);
        (stalls < stall_limit, last_live, stalls)
    } else {
        (true, current, 0)
    }
}

/// Opaque safety-progress clock. Mining engines mark progress only after the
/// safety-critical loop (for example thermal sensing plus fan actuation) has
/// completed one iteration.
#[derive(Clone, Default)]
pub(crate) struct SafetyLiveness(Arc<AtomicU64>);

impl SafetyLiveness {
    pub(crate) fn mark_progress(&self) {
        self.0.fetch_add(1, Ordering::Release);
    }

    fn snapshot(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WatchdogArmReceipt {
    pub(crate) requested_timeout_s: u32,
    pub(crate) effective_timeout_s: u32,
    pub(crate) kick_interval_s: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WatchdogAdmission {
    Armed(WatchdogArmReceipt),
    DisabledByConfiguration,
    Unavailable { reason: String },
}

impl WatchdogAdmission {
    pub(crate) fn require_armed(self, engine: &'static str) -> Result<WatchdogArmReceipt> {
        match self {
            Self::Armed(receipt) => Ok(receipt),
            Self::DisabledByConfiguration => anyhow::bail!(
                "{engine} requires an armed SoC watchdog before energizing hardware; watchdog is disabled by configuration"
            ),
            Self::Unavailable { reason } => anyhow::bail!(
                "{engine} requires an armed SoC watchdog before energizing hardware: {reason}"
            ),
        }
    }
}

mod evidence_sealed {
    pub trait Sealed {}
}

/// Owner-issued proof that every registered hardware-mutating actor has
/// terminated. Implementations are centrally sealed receipt types, never
/// engine-provided booleans.
pub(crate) trait ActorQuiescenceEvidence: evidence_sealed::Sealed {
    fn all_hardware_actors_quiesced(&self) -> bool;
}

/// Owner-issued proof that new hardware mutations are rejected and every
/// previously admitted mutation has completed.
pub(crate) trait MutationBarrierEvidence: evidence_sealed::Sealed {
    fn hardware_mutations_closed_and_drained(&self) -> bool;
}

/// HAL-issued proof that its software safe-off command and required
/// readback completed. This is command/readback evidence, not physical rail
/// measurement.
pub(crate) trait SoftwareSafeOffEvidence: evidence_sealed::Sealed {
    fn software_safe_off_completed(&self) -> bool;
}

impl evidence_sealed::Sealed for ThreadStopSummary {}

impl ActorQuiescenceEvidence for ThreadStopSummary {
    fn all_hardware_actors_quiesced(&self) -> bool {
        !self.any_timed_out()
    }
}

impl evidence_sealed::Sealed for dcentrald_hal::platform::HardwareMutationBarrierReceipt {}

impl MutationBarrierEvidence for dcentrald_hal::platform::HardwareMutationBarrierReceipt {
    fn hardware_mutations_closed_and_drained(&self) -> bool {
        true
    }
}

impl evidence_sealed::Sealed for dcentrald_hal::i2c::TerminalSafeOffTransition {}

impl MutationBarrierEvidence for dcentrald_hal::i2c::TerminalSafeOffTransition {
    fn hardware_mutations_closed_and_drained(&self) -> bool {
        self.no_controller_mutation_stage_in_flight()
    }
}

impl evidence_sealed::Sealed for dcentrald_hal::platform::amlogic::PsuSafeOffReceipt {}

impl SoftwareSafeOffEvidence for dcentrald_hal::platform::amlogic::PsuSafeOffReceipt {
    fn software_safe_off_completed(&self) -> bool {
        true
    }
}

impl evidence_sealed::Sealed for crate::am3_bb_mining::Am3BbSafeOffReceipt {}

impl SoftwareSafeOffEvidence for crate::am3_bb_mining::Am3BbSafeOffReceipt {
    fn software_safe_off_completed(&self) -> bool {
        true
    }
}

/// Private-construction capability required for magic-close.
pub(crate) struct WatchdogDisarmPermit {
    _private: (),
}

impl WatchdogDisarmPermit {
    pub(crate) fn from_evidence<M, Q, S>(
        mutation_barrier: &M,
        quiescence: &Q,
        safe_off: &S,
    ) -> Result<Self>
    where
        M: MutationBarrierEvidence,
        Q: ActorQuiescenceEvidence,
        S: SoftwareSafeOffEvidence,
    {
        let mutation_barriers: [&dyn MutationBarrierEvidence; 1] = [mutation_barrier];
        Self::from_evidence_set(&mutation_barriers, quiescence, safe_off)
    }

    /// Mint a disarm capability only from every independently owned mutation
    /// domain. Engines such as AM3-BB have both API and controller-service
    /// admission barriers; collapsing them into one boolean would permit a
    /// future caller to accidentally omit an entire mutation plane.
    pub(crate) fn from_evidence_set<Q, S>(
        mutation_barriers: &[&dyn MutationBarrierEvidence],
        quiescence: &Q,
        safe_off: &S,
    ) -> Result<Self>
    where
        Q: ActorQuiescenceEvidence,
        S: SoftwareSafeOffEvidence,
    {
        if mutation_barriers.is_empty() {
            anyhow::bail!("watchdog disarm requires at least one mutation barrier receipt");
        }
        for (index, mutation_barrier) in mutation_barriers.iter().enumerate() {
            if !mutation_barrier.hardware_mutations_closed_and_drained() {
                anyhow::bail!("hardware mutation barrier evidence at index {index} is incomplete");
            }
        }
        if !quiescence.all_hardware_actors_quiesced() {
            anyhow::bail!("hardware actor quiescence evidence is incomplete");
        }
        if !safe_off.software_safe_off_completed() {
            anyhow::bail!("software safe-off evidence is incomplete");
        }
        Ok(Self { _private: () })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WatchdogCloseoutReceipt {
    MagicCloseWriteCompletedAndWorkerExitObserved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FeedSuppressionReason {
    BringupDeadlineExpired,
    MiningLivenessStalled,
    TeardownDeadlineExpired,
}

#[derive(Debug)]
enum WatchdogPhase {
    Bringup { deadline: Instant },
    Mining { last_live: u64, stalls: u64 },
    Teardown { deadline: Instant },
    FeedSuppressed(FeedSuppressionReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhaseReceipt {
    MiningAdmitted,
    TeardownAdmitted,
}

enum WatchdogCommand {
    EnterMining {
        reply: oneshot::Sender<std::result::Result<PhaseReceipt, String>>,
    },
    BeginTeardown {
        deadline: Instant,
        reply: oneshot::Sender<std::result::Result<PhaseReceipt, String>>,
    },
    Disarm {
        _permit: WatchdogDisarmPermit,
        reply: oneshot::Sender<std::result::Result<(), String>>,
    },
}

trait WatchdogDevice: Send {
    fn set_timeout(&self, seconds: u32) -> std::result::Result<u32, String>;
    fn kick(&self) -> std::result::Result<(), String>;
    fn close_magic(self: Box<Self>) -> std::result::Result<(), String>;
}

impl WatchdogDevice for Watchdog {
    fn set_timeout(&self, seconds: u32) -> std::result::Result<u32, String> {
        #[cfg(unix)]
        {
            Watchdog::set_timeout(self, seconds).map_err(|error| error.to_string())
        }
        #[cfg(not(unix))]
        {
            Ok(seconds)
        }
    }

    fn kick(&self) -> std::result::Result<(), String> {
        Watchdog::kick(self).map_err(|error| error.to_string())
    }

    fn close_magic(self: Box<Self>) -> std::result::Result<(), String> {
        Watchdog::close_magic(*self).map_err(|error| error.to_string())
    }
}

type WatchdogFactory =
    Box<dyn FnOnce() -> std::result::Result<Box<dyn WatchdogDevice>, String> + Send>;

/// Sole owner of one fail-closed watchdog worker.
pub(crate) struct SafetyWatchdogOwner {
    command_tx: Option<mpsc::Sender<WatchdogCommand>>,
    worker: Option<JoinHandle<()>>,
    teardown_admitted: bool,
}

impl SafetyWatchdogOwner {
    pub(crate) async fn start_before_energizing(
        config: &WatchdogConfig,
        bringup_grace: Duration,
        expected_liveness_interval: Duration,
        liveness: SafetyLiveness,
    ) -> Result<(Self, WatchdogAdmission)> {
        Self::start_with_factory(
            config,
            bringup_grace,
            expected_liveness_interval,
            liveness,
            Box::new(|| {
                Watchdog::open()
                    .map(|watchdog| Box::new(watchdog) as Box<dyn WatchdogDevice>)
                    .map_err(|error| error.to_string())
            }),
        )
        .await
    }

    async fn start_with_factory(
        config: &WatchdogConfig,
        bringup_grace: Duration,
        expected_liveness_interval: Duration,
        liveness: SafetyLiveness,
        factory: WatchdogFactory,
    ) -> Result<(Self, WatchdogAdmission)> {
        if !config.enabled {
            return Ok((
                Self {
                    command_tx: None,
                    worker: None,
                    teardown_admitted: false,
                },
                WatchdogAdmission::DisabledByConfiguration,
            ));
        }
        if bringup_grace.is_zero() {
            anyhow::bail!("watchdog bring-up grace must be non-zero");
        }

        let (command_tx, command_rx) = mpsc::channel();
        let (admission_tx, admission_rx) = oneshot::channel();
        let config = config.clone();
        let worker = std::thread::Builder::new()
            .name("soc-safety-watchdog".to_string())
            .spawn(move || {
                watchdog_worker(
                    config,
                    bringup_grace,
                    expected_liveness_interval,
                    liveness,
                    command_rx,
                    admission_tx,
                    factory,
                );
            })
            .context("failed to spawn SoC safety-watchdog owner thread")?;

        let mut owner = Self {
            command_tx: Some(command_tx),
            worker: Some(worker),
            teardown_admitted: false,
        };
        let admission = match tokio::time::timeout(WATCHDOG_ADMISSION_TIMEOUT, admission_rx).await {
            Ok(Ok(admission)) => admission,
            Ok(Err(_)) => {
                owner.command_tx.take();
                let _ = owner.join_worker(DEFAULT_WATCHDOG_STOP_TIMEOUT).await;
                anyhow::bail!("SoC watchdog worker exited without an arm admission");
            }
            Err(_) => {
                owner.command_tx.take();
                let _ = owner.join_worker(DEFAULT_WATCHDOG_STOP_TIMEOUT).await;
                anyhow::bail!("timed out waiting for SoC watchdog arm admission");
            }
        };

        if !matches!(admission, WatchdogAdmission::Armed(_)) {
            owner.command_tx.take();
            owner.join_worker(DEFAULT_WATCHDOG_STOP_TIMEOUT).await?;
        }
        Ok((owner, admission))
    }

    pub(crate) async fn enter_mining(&mut self) -> Result<()> {
        let tx = self
            .command_tx
            .as_ref()
            .context("watchdog was not armed by this daemon")?;
        let (reply, receipt) = oneshot::channel();
        tx.send(WatchdogCommand::EnterMining { reply })
            .map_err(|_| {
                anyhow::anyhow!("watchdog command channel closed before Mining admission")
            })?;
        match tokio::time::timeout(WATCHDOG_ADMISSION_TIMEOUT, receipt).await {
            Ok(Ok(Ok(PhaseReceipt::MiningAdmitted))) => Ok(()),
            Ok(Ok(Ok(other))) => anyhow::bail!("unexpected watchdog phase receipt: {other:?}"),
            Ok(Ok(Err(reason))) => anyhow::bail!("watchdog refused Mining admission: {reason}"),
            Ok(Err(_)) => anyhow::bail!("watchdog worker exited before Mining admission"),
            Err(_) => anyhow::bail!("timed out waiting for watchdog Mining admission"),
        }
    }

    pub(crate) async fn begin_teardown(&mut self, grace: Duration) -> Result<()> {
        if self.teardown_admitted {
            anyhow::bail!("watchdog teardown was already admitted; refusing deadline extension");
        }
        if grace.is_zero() {
            anyhow::bail!("watchdog teardown grace must be non-zero");
        }
        let tx = self
            .command_tx
            .as_ref()
            .context("watchdog was not armed by this daemon")?;
        let (reply, receipt) = oneshot::channel();
        tx.send(WatchdogCommand::BeginTeardown {
            deadline: Instant::now() + grace,
            reply,
        })
        .map_err(|_| {
            anyhow::anyhow!("watchdog command channel closed before Teardown admission")
        })?;
        match tokio::time::timeout(WATCHDOG_ADMISSION_TIMEOUT, receipt).await {
            Ok(Ok(Ok(PhaseReceipt::TeardownAdmitted))) => {
                self.teardown_admitted = true;
                Ok(())
            }
            Ok(Ok(Ok(other))) => anyhow::bail!("unexpected watchdog phase receipt: {other:?}"),
            Ok(Ok(Err(reason))) => anyhow::bail!("watchdog refused Teardown admission: {reason}"),
            Ok(Err(_)) => anyhow::bail!("watchdog worker exited before Teardown admission"),
            Err(_) => anyhow::bail!("timed out waiting for watchdog Teardown admission"),
        }
    }

    pub(crate) async fn disarm_and_join(
        mut self,
        permit: WatchdogDisarmPermit,
        timeout: Duration,
    ) -> Result<WatchdogCloseoutReceipt> {
        if !self.teardown_admitted {
            anyhow::bail!("watchdog Disarm requires an actor-observed Teardown admission");
        }
        let tx = self
            .command_tx
            .as_ref()
            .context("watchdog was not armed by this daemon")?;
        let (reply, receipt) = oneshot::channel();
        tx.send(WatchdogCommand::Disarm {
            _permit: permit,
            reply,
        })
        .map_err(|_| anyhow::anyhow!("watchdog command channel closed before Disarm"))?;

        match tokio::time::timeout(timeout, receipt).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(reason))) => anyhow::bail!("watchdog refused or failed Disarm: {reason}"),
            Ok(Err(_)) => {
                self.command_tx.take();
                let worker_diagnostic = match self.join_worker(timeout).await {
                    Ok(()) => "worker exit was observed without a receipt".to_string(),
                    Err(error) => format!("worker termination diagnostic: {error:#}"),
                };
                anyhow::bail!(
                    "watchdog worker exited without a magic-close receipt; magic-close outcome is unknown; {worker_diagnostic}"
                );
            }
            Err(_) => anyhow::bail!(
                "timed out after requesting watchdog Disarm; magic-close outcome is unknown"
            ),
        }

        self.command_tx.take();
        self.join_worker(timeout).await?;
        Ok(WatchdogCloseoutReceipt::MagicCloseWriteCompletedAndWorkerExitObserved)
    }

    async fn join_worker(&mut self, timeout: Duration) -> Result<()> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        match join_thread_bounded(worker, timeout).await {
            ThreadStopOutcome::Joined => Ok(()),
            ThreadStopOutcome::Panicked => anyhow::bail!("SoC watchdog worker panicked"),
            ThreadStopOutcome::TimedOut => anyhow::bail!(
                "SoC watchdog worker termination was not observed before the deadline"
            ),
        }
    }
}

impl Drop for SafetyWatchdogOwner {
    fn drop(&mut self) {
        // Sender loss is the abnormal-stop command. The worker drops its device
        // without magic close. Dropping the JoinHandle detaches only until that
        // channel loss is observed; it never grants disarm authority.
        self.command_tx.take();
        self.worker.take();
    }
}

fn watchdog_worker(
    config: WatchdogConfig,
    bringup_grace: Duration,
    expected_liveness_interval: Duration,
    liveness: SafetyLiveness,
    command_rx: mpsc::Receiver<WatchdogCommand>,
    admission_tx: oneshot::Sender<WatchdogAdmission>,
    factory: WatchdogFactory,
) {
    let watchdog = match factory() {
        Ok(watchdog) => watchdog,
        Err(reason) => {
            let _ = admission_tx.send(WatchdogAdmission::Unavailable { reason });
            return;
        }
    };
    let effective_timeout_s = match watchdog.set_timeout(config.timeout_s) {
        Ok(timeout) => timeout,
        Err(reason) => {
            let _ = admission_tx.send(WatchdogAdmission::Unavailable {
                reason: format!("failed to configure watchdog timeout: {reason}"),
            });
            return;
        }
    };
    let kick_secs = watchdog_interval_secs(config.kick_interval_s as u64);
    if effective_timeout_s as u64 <= kick_secs {
        let _ = admission_tx.send(WatchdogAdmission::Unavailable {
            reason: format!(
                "kernel effective watchdog timeout {effective_timeout_s}s is not greater than kick interval {kick_secs}s"
            ),
        });
        return;
    }
    if let Err(reason) = watchdog.kick() {
        let _ = admission_tx.send(WatchdogAdmission::Unavailable {
            reason: format!("initial watchdog kick failed: {reason}"),
        });
        return;
    }

    // Bringup time starts only after the FD is configured and the initial kick
    // has completed. Arm admission can therefore never describe an already
    // expired phase, even if device open/configuration was slow.
    let bringup_deadline = Instant::now() + bringup_grace;

    let receipt = WatchdogArmReceipt {
        requested_timeout_s: config.timeout_s,
        effective_timeout_s,
        kick_interval_s: kick_secs,
    };
    if admission_tx
        .send(WatchdogAdmission::Armed(receipt.clone()))
        .is_err()
    {
        warn!("watchdog admission owner disappeared; leaving watchdog armed");
        return;
    }
    info!(
        requested_timeout_s = receipt.requested_timeout_s,
        effective_timeout_s = receipt.effective_timeout_s,
        kick_interval_s = receipt.kick_interval_s,
        "fail-closed SoC watchdog admitted in bounded Bringup phase"
    );

    let stall_limit = watchdog_stall_limit(
        effective_timeout_s as u64,
        kick_secs,
        Some(expected_liveness_interval),
    );
    let mut watchdog = Some(watchdog);
    let mut phase = WatchdogPhase::Bringup {
        deadline: bringup_deadline,
    };
    let mut next_kick = Instant::now() + Duration::from_secs(kick_secs);

    loop {
        let now = Instant::now();
        latch_expired_deadline(&mut phase, now);
        let next_phase_deadline = match phase {
            WatchdogPhase::Bringup { deadline } | WatchdogPhase::Teardown { deadline } => {
                Some(deadline)
            }
            WatchdogPhase::Mining { .. } | WatchdogPhase::FeedSuppressed(_) => None,
        };
        let next_event = next_phase_deadline
            .map(|deadline| deadline.min(next_kick))
            .unwrap_or(next_kick);
        let wait = next_event.saturating_duration_since(now);

        match command_rx.recv_timeout(wait) {
            Ok(command) => {
                if handle_command(command, &mut phase, &liveness, &mut watchdog) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                warn!("watchdog command owner disappeared; leaving watchdog armed");
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                latch_expired_deadline(&mut phase, now);
                if now < next_kick {
                    continue;
                }

                let should_kick = match &mut phase {
                    WatchdogPhase::Bringup { .. } | WatchdogPhase::Teardown { .. } => true,
                    WatchdogPhase::Mining { last_live, stalls } => {
                        let current = liveness.snapshot();
                        let (should_kick, new_last, new_stalls) =
                            watchdog_kick_decision(current, *last_live, *stalls, stall_limit);
                        *last_live = new_last;
                        *stalls = new_stalls;
                        if !should_kick {
                            phase = WatchdogPhase::FeedSuppressed(
                                FeedSuppressionReason::MiningLivenessStalled,
                            );
                            error!(
                                stalls = new_stalls,
                                stall_limit,
                                "watchdog safety liveness stalled; feed suppression is terminal"
                            );
                        }
                        should_kick
                    }
                    WatchdogPhase::FeedSuppressed(reason) => {
                        error!(?reason, "watchdog feed remains terminally suppressed");
                        false
                    }
                };
                if should_kick {
                    if let Some(device) = watchdog.as_ref() {
                        if let Err(reason) = device.kick() {
                            error!(%reason, "watchdog kick failed; feed loop continues but reset may occur");
                        }
                    }
                }
                next_kick = now + Duration::from_secs(kick_secs);
            }
        }
    }
}

fn latch_expired_deadline(phase: &mut WatchdogPhase, now: Instant) {
    let reason = match phase {
        WatchdogPhase::Bringup { deadline } if !watchdog_teardown_kick_allowed(*deadline, now) => {
            Some(FeedSuppressionReason::BringupDeadlineExpired)
        }
        WatchdogPhase::Teardown { deadline } if !watchdog_teardown_kick_allowed(*deadline, now) => {
            Some(FeedSuppressionReason::TeardownDeadlineExpired)
        }
        _ => None,
    };
    if let Some(reason) = reason {
        error!(
            ?reason,
            "watchdog deadline expired; feed suppression is terminal"
        );
        *phase = WatchdogPhase::FeedSuppressed(reason);
    }
}

/// Returns true only after an explicit magic-close attempt, so the worker exits.
fn handle_command(
    command: WatchdogCommand,
    phase: &mut WatchdogPhase,
    liveness: &SafetyLiveness,
    watchdog: &mut Option<Box<dyn WatchdogDevice>>,
) -> bool {
    latch_expired_deadline(phase, Instant::now());
    match command {
        WatchdogCommand::EnterMining { reply } => {
            let result = match phase {
                WatchdogPhase::Bringup { .. } => {
                    *phase = WatchdogPhase::Mining {
                        last_live: liveness.snapshot(),
                        stalls: 0,
                    };
                    Ok(PhaseReceipt::MiningAdmitted)
                }
                WatchdogPhase::FeedSuppressed(reason) => {
                    Err(format!("feed already suppressed: {reason:?}"))
                }
                other => Err(format!("invalid phase for Mining admission: {other:?}")),
            };
            let _ = reply.send(result);
            false
        }
        WatchdogCommand::BeginTeardown { deadline, reply } => {
            let result = if deadline <= Instant::now() {
                Err("teardown deadline is not in the future".to_string())
            } else {
                match phase {
                    WatchdogPhase::Bringup { .. } | WatchdogPhase::Mining { .. } => {
                        *phase = WatchdogPhase::Teardown { deadline };
                        Ok(PhaseReceipt::TeardownAdmitted)
                    }
                    WatchdogPhase::Teardown { .. } => {
                        Err("teardown already admitted; deadline extension refused".to_string())
                    }
                    WatchdogPhase::FeedSuppressed(reason) => {
                        Err(format!("feed already suppressed: {reason:?}"))
                    }
                }
            };
            let _ = reply.send(result);
            false
        }
        WatchdogCommand::Disarm { _permit: _, reply } => {
            // The teardown deadline is the latest time at which the worker may
            // *admit* the non-cancellable kernel magic-close write. Character-
            // device completion is then reported separately and the owner must
            // still observe worker exit. A blocking write cannot be revoked
            // safely after it begins, so this is intentionally not described as
            // a completion deadline.
            let result = match phase {
                WatchdogPhase::Teardown { deadline } if *deadline > Instant::now() => watchdog
                    .take()
                    .ok_or_else(|| "watchdog device was already consumed".to_string())
                    .and_then(WatchdogDevice::close_magic),
                WatchdogPhase::FeedSuppressed(reason) => Err(format!(
                    "terminal feed suppression forbids Disarm: {reason:?}"
                )),
                other => Err(format!("invalid phase for Disarm: {other:?}")),
            };
            let _ = reply.send(result);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeState {
        events: Mutex<Vec<&'static str>>,
        close_fails: AtomicBool,
        panic_on_close: AtomicBool,
        kick_fails: AtomicBool,
        panic_on_set_timeout: AtomicBool,
        drops_armed: AtomicUsize,
    }

    struct FakeWatchdog {
        state: Arc<FakeState>,
        closed: bool,
    }

    impl Drop for FakeWatchdog {
        fn drop(&mut self) {
            if !self.closed {
                self.state.drops_armed.fetch_add(1, Ordering::SeqCst);
                self.state.events.lock().unwrap().push("drop-armed");
            }
        }
    }

    impl WatchdogDevice for FakeWatchdog {
        fn set_timeout(&self, seconds: u32) -> std::result::Result<u32, String> {
            self.state.events.lock().unwrap().push("set-timeout");
            if self.state.panic_on_set_timeout.load(Ordering::SeqCst) {
                panic!("scripted watchdog worker panic");
            }
            Ok(seconds)
        }

        fn kick(&self) -> std::result::Result<(), String> {
            self.state.events.lock().unwrap().push("kick");
            if self.state.kick_fails.load(Ordering::SeqCst) {
                return Err("scripted kick failure".to_string());
            }
            Ok(())
        }

        fn close_magic(mut self: Box<Self>) -> std::result::Result<(), String> {
            self.state.events.lock().unwrap().push("magic-close");
            if self.state.panic_on_close.load(Ordering::SeqCst) {
                panic!("scripted magic-close panic");
            }
            if self.state.close_fails.load(Ordering::SeqCst) {
                return Err("scripted close failure".to_string());
            }
            self.closed = true;
            Ok(())
        }
    }

    struct CompleteActors;
    impl evidence_sealed::Sealed for CompleteActors {}
    impl ActorQuiescenceEvidence for CompleteActors {
        fn all_hardware_actors_quiesced(&self) -> bool {
            true
        }
    }
    struct CompleteMutations;
    impl evidence_sealed::Sealed for CompleteMutations {}
    impl MutationBarrierEvidence for CompleteMutations {
        fn hardware_mutations_closed_and_drained(&self) -> bool {
            true
        }
    }
    struct CompleteSafeOff;
    impl evidence_sealed::Sealed for CompleteSafeOff {}
    impl SoftwareSafeOffEvidence for CompleteSafeOff {
        fn software_safe_off_completed(&self) -> bool {
            true
        }
    }
    struct Incomplete;
    impl evidence_sealed::Sealed for Incomplete {}
    impl ActorQuiescenceEvidence for Incomplete {
        fn all_hardware_actors_quiesced(&self) -> bool {
            false
        }
    }
    impl SoftwareSafeOffEvidence for Incomplete {
        fn software_safe_off_completed(&self) -> bool {
            false
        }
    }
    impl MutationBarrierEvidence for Incomplete {
        fn hardware_mutations_closed_and_drained(&self) -> bool {
            false
        }
    }

    fn config() -> WatchdogConfig {
        WatchdogConfig {
            enabled: true,
            timeout_s: 30,
            kick_interval_s: 5,
        }
    }

    async fn fake_owner(
        state: Arc<FakeState>,
        bringup_grace: Duration,
    ) -> (SafetyWatchdogOwner, SafetyLiveness) {
        let liveness = SafetyLiveness::default();
        let factory_state = Arc::clone(&state);
        let (owner, admission) = SafetyWatchdogOwner::start_with_factory(
            &config(),
            bringup_grace,
            Duration::from_secs(2),
            liveness.clone(),
            Box::new(move || {
                Ok(Box::new(FakeWatchdog {
                    state: factory_state,
                    closed: false,
                }))
            }),
        )
        .await
        .unwrap();
        assert!(matches!(admission, WatchdogAdmission::Armed(_)));
        (owner, liveness)
    }

    #[test]
    fn mining_stall_decision_has_no_zero_is_healthy_sentinel() {
        assert_eq!(watchdog_kick_decision(0, 0, 0, 2), (true, 0, 1));
        assert_eq!(watchdog_kick_decision(0, 0, 1, 2), (false, 0, 2));
    }

    #[test]
    fn expired_deadline_latches_and_cannot_return_to_mining() {
        let mut phase = WatchdogPhase::Bringup {
            deadline: Instant::now(),
        };
        latch_expired_deadline(&mut phase, Instant::now());
        assert!(matches!(phase, WatchdogPhase::FeedSuppressed(_)));
        let (reply, mut receipt) = oneshot::channel();
        let mut device: Option<Box<dyn WatchdogDevice>> = None;
        assert!(!handle_command(
            WatchdogCommand::EnterMining { reply },
            &mut phase,
            &SafetyLiveness::default(),
            &mut device,
        ));
        assert!(receipt.try_recv().unwrap().is_err());
    }

    #[test]
    fn expired_teardown_rejects_late_disarm_without_magic_close() {
        let state = Arc::new(FakeState::default());
        let mut phase = WatchdogPhase::Teardown {
            deadline: Instant::now(),
        };
        let mut device: Option<Box<dyn WatchdogDevice>> = Some(Box::new(FakeWatchdog {
            state: Arc::clone(&state),
            closed: false,
        }));
        let permit = WatchdogDisarmPermit::from_evidence(
            &CompleteMutations,
            &CompleteActors,
            &CompleteSafeOff,
        )
        .unwrap();
        let (reply, mut receipt) = oneshot::channel();
        assert!(handle_command(
            WatchdogCommand::Disarm {
                _permit: permit,
                reply,
            },
            &mut phase,
            &SafetyLiveness::default(),
            &mut device,
        ));
        assert!(receipt.try_recv().unwrap().is_err());
        drop(device);
        assert!(!state.events.lock().unwrap().contains(&"magic-close"));
        assert_eq!(state.drops_armed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn owner_drop_and_command_loss_never_magic_close() {
        let state = Arc::new(FakeState::default());
        let (owner, _) = fake_owner(Arc::clone(&state), Duration::from_secs(30)).await;
        drop(owner);
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.drops_armed.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!state.events.lock().unwrap().contains(&"magic-close"));
    }

    #[tokio::test]
    async fn disabled_open_failure_and_initial_kick_failure_never_admit_arming() {
        let mut disabled = config();
        disabled.enabled = false;
        let factory_called = Arc::new(AtomicBool::new(false));
        let factory_called_worker = Arc::clone(&factory_called);
        let (_owner, admission) = SafetyWatchdogOwner::start_with_factory(
            &disabled,
            Duration::from_secs(30),
            Duration::from_secs(2),
            SafetyLiveness::default(),
            Box::new(move || {
                factory_called_worker.store(true, Ordering::SeqCst);
                Err("must not open".to_string())
            }),
        )
        .await
        .unwrap();
        assert_eq!(admission, WatchdogAdmission::DisabledByConfiguration);
        assert!(!factory_called.load(Ordering::SeqCst));
        assert!(admission.require_armed("test NoPic").is_err());

        let (_owner, admission) = SafetyWatchdogOwner::start_with_factory(
            &config(),
            Duration::from_secs(30),
            Duration::from_secs(2),
            SafetyLiveness::default(),
            Box::new(|| Err("scripted open failure".to_string())),
        )
        .await
        .unwrap();
        assert!(matches!(&admission, WatchdogAdmission::Unavailable { .. }));
        assert!(admission.require_armed("test NoPic").is_err());

        let state = Arc::new(FakeState::default());
        state.kick_fails.store(true, Ordering::SeqCst);
        let factory_state = Arc::clone(&state);
        let (_owner, admission) = SafetyWatchdogOwner::start_with_factory(
            &config(),
            Duration::from_secs(30),
            Duration::from_secs(2),
            SafetyLiveness::default(),
            Box::new(move || {
                Ok(Box::new(FakeWatchdog {
                    state: factory_state,
                    closed: false,
                }))
            }),
        )
        .await
        .unwrap();
        assert!(matches!(admission, WatchdogAdmission::Unavailable { .. }));
        assert_eq!(state.drops_armed.load(Ordering::SeqCst), 1);
        assert!(!state.events.lock().unwrap().contains(&"magic-close"));
    }

    #[tokio::test]
    async fn worker_panic_is_observed_without_magic_close() {
        let state = Arc::new(FakeState::default());
        state.panic_on_set_timeout.store(true, Ordering::SeqCst);
        let factory_state = Arc::clone(&state);
        let result = SafetyWatchdogOwner::start_with_factory(
            &config(),
            Duration::from_secs(30),
            Duration::from_secs(2),
            SafetyLiveness::default(),
            Box::new(move || {
                Ok(Box::new(FakeWatchdog {
                    state: factory_state,
                    closed: false,
                }))
            }),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(state.drops_armed.load(Ordering::SeqCst), 1);
        assert!(!state.events.lock().unwrap().contains(&"magic-close"));
    }

    #[tokio::test]
    async fn bringup_expiry_and_teardown_retry_are_terminal_or_nonextending() {
        let state = Arc::new(FakeState::default());
        let (mut expired_owner, _) =
            fake_owner(Arc::clone(&state), Duration::from_millis(20)).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(expired_owner.enter_mining().await.is_err());
        drop(expired_owner);

        let state = Arc::new(FakeState::default());
        let (mut owner, _) = fake_owner(state, Duration::from_secs(30)).await;
        owner.begin_teardown(Duration::from_secs(10)).await.unwrap();
        assert!(owner.begin_teardown(Duration::from_secs(20)).await.is_err());
        drop(owner);
    }

    #[tokio::test]
    async fn worker_liveness_stall_is_terminal_after_late_progress() {
        let state = Arc::new(FakeState::default());
        let factory_state = Arc::clone(&state);
        let liveness = SafetyLiveness::default();
        let mut fast_config = config();
        fast_config.timeout_s = 3;
        fast_config.kick_interval_s = 1;
        let (mut owner, admission) = SafetyWatchdogOwner::start_with_factory(
            &fast_config,
            Duration::from_secs(10),
            Duration::from_millis(10),
            liveness.clone(),
            Box::new(move || {
                Ok(Box::new(FakeWatchdog {
                    state: factory_state,
                    closed: false,
                }))
            }),
        )
        .await
        .unwrap();
        assert!(matches!(admission, WatchdogAdmission::Armed(_)));
        owner.enter_mining().await.unwrap();

        // stall_limit=3 after the cadence safety margin: the first two mining
        // intervals are kicked and the third terminally suppresses. A later
        // safety-loop advance must not resume.
        tokio::time::sleep(Duration::from_millis(3300)).await;
        let kicks_before_late_progress = state
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "kick")
            .count();
        liveness.mark_progress();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let kicks_after_late_progress = state
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "kick")
            .count();
        assert_eq!(kicks_after_late_progress, kicks_before_late_progress);
        assert!(owner.begin_teardown(Duration::from_secs(5)).await.is_err());
        drop(owner);
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.drops_armed.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!state.events.lock().unwrap().contains(&"magic-close"));
    }

    #[test]
    fn incomplete_engine_evidence_cannot_mint_disarm_permit() {
        assert!(WatchdogDisarmPermit::from_evidence(
            &Incomplete,
            &CompleteActors,
            &CompleteSafeOff,
        )
        .is_err());
        assert!(WatchdogDisarmPermit::from_evidence(
            &CompleteMutations,
            &Incomplete,
            &CompleteSafeOff,
        )
        .is_err());
        assert!(WatchdogDisarmPermit::from_evidence(
            &CompleteMutations,
            &CompleteActors,
            &Incomplete,
        )
        .is_err());
    }

    #[test]
    fn disarm_permit_requires_every_mutation_domain() {
        let complete: [&dyn MutationBarrierEvidence; 2] = [&CompleteMutations, &CompleteMutations];
        assert!(WatchdogDisarmPermit::from_evidence_set(
            &complete,
            &CompleteActors,
            &CompleteSafeOff,
        )
        .is_ok());

        let one_incomplete: [&dyn MutationBarrierEvidence; 2] = [&CompleteMutations, &Incomplete];
        assert!(WatchdogDisarmPermit::from_evidence_set(
            &one_incomplete,
            &CompleteActors,
            &CompleteSafeOff,
        )
        .is_err());
    }

    #[test]
    fn disarm_permit_rejects_an_empty_mutation_domain_set() {
        let empty: [&dyn MutationBarrierEvidence; 0] = [];
        assert!(
            WatchdogDisarmPermit::from_evidence_set(&empty, &CompleteActors, &CompleteSafeOff,)
                .is_err()
        );
    }

    #[tokio::test]
    async fn successful_disarm_requires_receipt_and_observed_worker_exit() {
        let state = Arc::new(FakeState::default());
        let (mut owner, liveness) = fake_owner(Arc::clone(&state), Duration::from_secs(30)).await;
        owner.enter_mining().await.unwrap();
        liveness.mark_progress();
        owner.begin_teardown(Duration::from_secs(10)).await.unwrap();
        let permit = WatchdogDisarmPermit::from_evidence(
            &CompleteMutations,
            &CompleteActors,
            &CompleteSafeOff,
        )
        .unwrap();
        let receipt = owner
            .disarm_and_join(permit, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            receipt,
            WatchdogCloseoutReceipt::MagicCloseWriteCompletedAndWorkerExitObserved
        );
        let events = state.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == "magic-close")
                .count(),
            1
        );
        assert_eq!(state.drops_armed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn magic_close_failure_never_mints_positive_closeout() {
        let state = Arc::new(FakeState::default());
        state.close_fails.store(true, Ordering::SeqCst);
        let (mut owner, _) = fake_owner(Arc::clone(&state), Duration::from_secs(30)).await;
        owner.enter_mining().await.unwrap();
        owner.begin_teardown(Duration::from_secs(10)).await.unwrap();
        let permit = WatchdogDisarmPermit::from_evidence(
            &CompleteMutations,
            &CompleteActors,
            &CompleteSafeOff,
        )
        .unwrap();
        assert!(owner
            .disarm_and_join(permit, Duration::from_secs(1))
            .await
            .is_err());
        assert_eq!(state.drops_armed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn magic_close_receipt_drop_reports_unknown_outcome_and_worker_panic() {
        let state = Arc::new(FakeState::default());
        state.panic_on_close.store(true, Ordering::SeqCst);
        let (mut owner, _) = fake_owner(Arc::clone(&state), Duration::from_secs(30)).await;
        owner.enter_mining().await.unwrap();
        owner.begin_teardown(Duration::from_secs(10)).await.unwrap();
        let permit = WatchdogDisarmPermit::from_evidence(
            &CompleteMutations,
            &CompleteActors,
            &CompleteSafeOff,
        )
        .unwrap();

        let error = owner
            .disarm_and_join(permit, Duration::from_secs(1))
            .await
            .expect_err("magic-close panic must not mint positive closeout")
            .to_string();
        assert!(error.contains("magic-close outcome is unknown"));
        assert!(error.contains("watchdog worker panicked"));
        assert_eq!(state.drops_armed.load(Ordering::SeqCst), 1);
    }
}
