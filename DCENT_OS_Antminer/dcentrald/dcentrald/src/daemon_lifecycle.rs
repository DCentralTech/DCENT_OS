//! Standard-daemon bring-up/recovery orchestration.
//!
//! The hardware implementation remains in `daemon.rs`, but the ordering contract
//! is expressed through this narrow port: initialize, stop initialization-only
//! keepalives, make partially initialized hardware safe, and only then enter the
//! management plane.  Keeping the coordinator independent of concrete UIO/I2C
//! constructors lets the simulator execute failure and timeout paths rather than
//! relying on lexical source assertions.

use std::future::Future;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tracing::error;

/// Immutable identity evidence captured before standard-daemon hardware access.
///
/// The field names deliberately preserve provenance. A board-target, platform
/// marker, or subtype file is *declared package metadata*; it is not measured
/// silicon identity. `observed_control_board` is inferred from OS-visible device
/// signatures and is likewise only control-board evidence. ASIC enumeration
/// remains the authority for the attached chip family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlatformIdentitySnapshot {
    pub(crate) declared_board_target: Option<String>,
    /// Exact registry row for the declared control-board target, when known.
    /// This remains control-board composition metadata; it does not identify
    /// measured ASIC silicon, hashboard SKU, PSU, cooling, storage, or network.
    pub(crate) board_desc: Option<&'static dcentrald_common::BoardDesc>,
    pub(crate) declared_platform_marker: Option<String>,
    pub(crate) declared_subtype: Option<String>,
    pub(crate) declared_psu_hardware_variant: Option<String>,
    pub(crate) observed_control_board: String,
}

impl PlatformIdentitySnapshot {
    pub(crate) fn board_target(&self) -> &str {
        self.declared_board_target.as_deref().unwrap_or_default()
    }

    pub(crate) fn platform_marker(&self) -> &str {
        self.declared_platform_marker.as_deref().unwrap_or_default()
    }

    pub(crate) fn subtype(&self) -> &str {
        self.declared_subtype.as_deref().unwrap_or_default()
    }

    pub(crate) fn psu_hardware_variant(&self) -> Option<&str> {
        self.declared_psu_hardware_variant.as_deref()
    }
}

/// Read-only capability that captures platform identity evidence once.
///
/// Implementations must not open I2C, GPIO, UIO, fan, PSU, or ASIC transports.
/// A single immutable result is shared by destructive-write admission and the
/// initial transport selection so a concurrently replaced board-target file
/// cannot produce contradictory decisions at those two safety boundaries.
pub(crate) trait PlatformIdentitySource {
    fn capture_identity(&self) -> Result<PlatformIdentitySnapshot>;
}

/// Result of applying a deadline to one lifecycle operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeadlineResult<T> {
    Completed(T),
    TimedOut,
}

/// Injectable deadline source for platform lifecycle tests.
///
/// Production uses Tokio's monotonic clock. Tests may choose a timeout without
/// sleeping or depending on host scheduling, which makes the cancellation edge
/// deterministic.
#[allow(async_fn_in_trait)]
pub(crate) trait LifecycleClock {
    async fn within<F>(&self, duration: Duration, future: F) -> DeadlineResult<F::Output>
    where
        F: Future;
}

/// Production monotonic deadline implementation.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TokioLifecycleClock;

impl LifecycleClock for TokioLifecycleClock {
    async fn within<F>(&self, duration: Duration, future: F) -> DeadlineResult<F::Output>
    where
        F: Future,
    {
        match tokio::time::timeout(duration, future).await {
            Ok(output) => DeadlineResult::Completed(output),
            Err(_) => DeadlineResult::TimedOut,
        }
    }
}

/// Hardware-facing lifecycle port owned by the standard daemon.
///
/// This is intentionally smaller than `dcentrald_hal::platform::Platform`.
/// Initialization currently spans legacy HAL surfaces that cannot be replaced
/// atomically. The port first makes the safety-critical *lifecycle* injectable;
/// individual UIO/I2C/GPIO constructors can migrate behind the HAL platform in
/// later, independently testable slices.
#[allow(async_fn_in_trait)]
pub(crate) trait PlatformLifecycle {
    async fn initialize_platform(&mut self, identity: &PlatformIdentitySnapshot) -> Result<()>;

    /// Stop initialization-only keepalives before attempting safe-off.
    fn stop_initialization_keepalives(&mut self);

    /// Best-effort safe-off for any partially initialized platform state.
    async fn safe_off_partial_platform(&mut self) -> Result<()>;

    /// Run the management plane until its normal shutdown condition.
    async fn run_management_only(&mut self) -> Result<()>;
}

/// Recovery-state publication capability used after failed hardware bring-up.
///
/// Recovery publication is deliberately separate from [`PlatformLifecycle`]:
/// it does not own a bus, rail, fan, watchdog, or any other hardware resource.
/// Implementations must be non-blocking (for example, an in-memory snapshot or
/// bounded channel send); persistent I/O belongs in a downstream async owner.
/// That owner can therefore be injected and tested without widening the
/// platform HAL or changing hardware wire ordering.
pub(crate) trait RecoveryPublisher {
    /// Publish the failed bring-up transition before management services start.
    ///
    /// Publication is advisory. Failure must be observable, but it must not
    /// prevent the management plane from becoming reachable.
    fn publish_management_recovery(&mut self) -> Result<()>;
}

/// Whether the caller may continue into the mining runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BringupDisposition {
    Ready,
    ManagementOnlyStopped,
}

/// Initialize a platform under a deadline and execute the complete recovery
/// handoff on failure.
///
/// Safe-off errors are logged but never suppress the management plane: a
/// controller watchdog may be the remaining hardware safety net, while keeping
/// the API reachable is still preferable to exiting the daemon. Failure of the
/// management plane itself remains an error.
pub(crate) async fn initialize_or_recover<P, R, C>(
    platform: &mut P,
    identity: &PlatformIdentitySnapshot,
    recovery_publisher: &mut R,
    clock: &C,
    init_timeout: Duration,
) -> Result<BringupDisposition>
where
    P: PlatformLifecycle,
    R: RecoveryPublisher,
    C: LifecycleClock,
{
    let init_result = match clock
        .within(init_timeout, platform.initialize_platform(identity))
        .await
    {
        DeadlineResult::Completed(result) => result,
        DeadlineResult::TimedOut => Err(anyhow!(
            "hardware bring-up (init) did not complete within {}s — the cold-boot \
             path is wedged (PIC/AXI-IIC/chip-UART timeout, PSU fault, or no \
             hash boards). Aborting bring-up so the management plane recovers.",
            init_timeout.as_secs()
        )),
    };

    let Err(error) = init_result else {
        return Ok(BringupDisposition::Ready);
    };

    error!(
        error = %error,
        timeout_s = init_timeout.as_secs(),
        "HARDWARE BRING-UP FAILED — running graceful hardware-safe-off \
         teardown, then falling back to MANAGEMENT-ONLY (API/dashboard \
         stay reachable; mining disabled until the operator acts). The \
         daemon will NOT hang and will NOT crash on a failed bring-up."
    );

    // Ordering is load-bearing: never keep a rail alive while waiting for a
    // transport that may itself be wedged during safe-off.
    platform.stop_initialization_keepalives();
    if let Err(teardown_error) = platform.safe_off_partial_platform().await {
        error!(
            error = %error,
            teardown_error = %teardown_error,
            "graceful teardown after bring-up failure also errored — the PIC/dsPIC \
             heartbeat watchdog (~5-64s) remains the hardware safety net"
        );
    }

    if let Err(publication_error) = recovery_publisher.publish_management_recovery() {
        error!(
            error = %publication_error,
            "failed to publish hardware bring-up recovery state; continuing to the management plane"
        );
    }
    platform.run_management_only().await?;
    Ok(BringupDisposition::ManagementOnlyStopped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn test_identity() -> PlatformIdentitySnapshot {
        PlatformIdentitySnapshot {
            declared_board_target: Some("am1-s9".to_string()),
            board_desc: dcentrald_common::BoardDesc::lookup("am1-s9"),
            declared_platform_marker: Some("zynq-bm1-s9".to_string()),
            declared_subtype: None,
            declared_psu_hardware_variant: None,
            observed_control_board: "Zynq am1-s9".to_string(),
        }
    }

    #[derive(Clone, Copy)]
    enum InitBehavior {
        Ready,
        Fail,
        Pending,
    }

    struct RecordingPlatform {
        events: Arc<Mutex<Vec<&'static str>>>,
        init: InitBehavior,
        safe_off_fails: bool,
        management_fails: bool,
    }

    impl RecordingPlatform {
        fn new(init: InitBehavior) -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                init,
                safe_off_fails: false,
                management_fails: false,
            }
        }

        fn record(&self, event: &'static str) {
            self.events.lock().unwrap().push(event);
        }

        fn snapshot(&self) -> Vec<&'static str> {
            self.events.lock().unwrap().clone()
        }
    }

    impl PlatformLifecycle for RecordingPlatform {
        async fn initialize_platform(&mut self, identity: &PlatformIdentitySnapshot) -> Result<()> {
            assert_eq!(identity, &test_identity());
            self.record("initialize");
            match self.init {
                InitBehavior::Ready => Ok(()),
                InitBehavior::Fail => Err(anyhow!("injected platform failure")),
                InitBehavior::Pending => std::future::pending().await,
            }
        }

        fn stop_initialization_keepalives(&mut self) {
            self.record("stop-keepalives");
        }

        async fn safe_off_partial_platform(&mut self) -> Result<()> {
            self.record("safe-off");
            if self.safe_off_fails {
                Err(anyhow!("injected safe-off failure"))
            } else {
                Ok(())
            }
        }

        async fn run_management_only(&mut self) -> Result<()> {
            self.record("management-only");
            if self.management_fails {
                Err(anyhow!("injected management failure"))
            } else {
                Ok(())
            }
        }
    }

    struct RecordingRecoveryPublisher {
        events: Arc<Mutex<Vec<&'static str>>>,
        fails: bool,
    }

    impl RecordingRecoveryPublisher {
        fn for_platform(platform: &RecordingPlatform) -> Self {
            Self {
                events: Arc::clone(&platform.events),
                fails: false,
            }
        }
    }

    impl RecoveryPublisher for RecordingRecoveryPublisher {
        fn publish_management_recovery(&mut self) -> Result<()> {
            self.events.lock().unwrap().push("mark-recovery");
            if self.fails {
                Err(anyhow!("injected recovery publication failure"))
            } else {
                Ok(())
            }
        }
    }

    struct ImmediateClock;

    impl LifecycleClock for ImmediateClock {
        async fn within<F>(&self, _duration: Duration, future: F) -> DeadlineResult<F::Output>
        where
            F: Future,
        {
            DeadlineResult::Completed(future.await)
        }
    }

    /// Deterministic deadline: allow initialization to make one unit of
    /// progress, then classify a pending future as timed out and drop it.
    /// This models cancellation after partial hardware mutation without a
    /// scheduler race or wall-clock sleep.
    struct PollOnceTimeoutClock;

    impl LifecycleClock for PollOnceTimeoutClock {
        async fn within<F>(&self, _duration: Duration, future: F) -> DeadlineResult<F::Output>
        where
            F: Future,
        {
            let mut future = std::pin::pin!(future);
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(output) => DeadlineResult::Completed(output),
                std::task::Poll::Pending => DeadlineResult::TimedOut,
            }
        }
    }

    #[tokio::test]
    async fn platform_identity_snapshot_reaches_successful_lifecycle_unchanged() {
        let mut platform = RecordingPlatform::new(InitBehavior::Ready);
        let mut publisher = RecordingRecoveryPublisher::for_platform(&platform);
        let disposition = initialize_or_recover(
            &mut platform,
            &test_identity(),
            &mut publisher,
            &ImmediateClock,
            Duration::from_secs(90),
        )
        .await
        .unwrap();

        assert_eq!(disposition, BringupDisposition::Ready);
        assert_eq!(platform.snapshot(), ["initialize"]);
    }

    #[tokio::test]
    async fn init_error_safe_off_precedes_management_handoff() {
        let mut platform = RecordingPlatform::new(InitBehavior::Fail);
        let mut publisher = RecordingRecoveryPublisher::for_platform(&platform);
        let disposition = initialize_or_recover(
            &mut platform,
            &test_identity(),
            &mut publisher,
            &ImmediateClock,
            Duration::from_secs(90),
        )
        .await
        .unwrap();

        assert_eq!(disposition, BringupDisposition::ManagementOnlyStopped);
        assert_eq!(
            platform.snapshot(),
            [
                "initialize",
                "stop-keepalives",
                "safe-off",
                "mark-recovery",
                "management-only",
            ]
        );
    }

    #[tokio::test]
    async fn injected_deadline_drops_hung_init_and_recovers_without_wall_clock_wait() {
        let mut platform = RecordingPlatform::new(InitBehavior::Pending);
        let mut publisher = RecordingRecoveryPublisher::for_platform(&platform);
        let disposition = initialize_or_recover(
            &mut platform,
            &test_identity(),
            &mut publisher,
            &PollOnceTimeoutClock,
            Duration::from_secs(90),
        )
        .await
        .unwrap();

        assert_eq!(disposition, BringupDisposition::ManagementOnlyStopped);
        assert_eq!(
            platform.snapshot(),
            [
                "initialize",
                "stop-keepalives",
                "safe-off",
                "mark-recovery",
                "management-only",
            ]
        );
    }

    #[tokio::test]
    async fn safe_off_error_does_not_make_the_management_plane_unreachable() {
        let mut platform = RecordingPlatform::new(InitBehavior::Fail);
        platform.safe_off_fails = true;
        let mut publisher = RecordingRecoveryPublisher::for_platform(&platform);
        let disposition = initialize_or_recover(
            &mut platform,
            &test_identity(),
            &mut publisher,
            &ImmediateClock,
            Duration::from_secs(90),
        )
        .await
        .unwrap();

        assert_eq!(disposition, BringupDisposition::ManagementOnlyStopped);
        assert_eq!(platform.snapshot().last(), Some(&"management-only"));
    }

    #[tokio::test]
    async fn recovery_publication_error_does_not_make_management_unreachable() {
        let mut platform = RecordingPlatform::new(InitBehavior::Fail);
        let mut publisher = RecordingRecoveryPublisher::for_platform(&platform);
        publisher.fails = true;
        let disposition = initialize_or_recover(
            &mut platform,
            &test_identity(),
            &mut publisher,
            &ImmediateClock,
            Duration::from_secs(90),
        )
        .await
        .unwrap();

        assert_eq!(disposition, BringupDisposition::ManagementOnlyStopped);
        assert_eq!(
            platform.snapshot(),
            [
                "initialize",
                "stop-keepalives",
                "safe-off",
                "mark-recovery",
                "management-only",
            ]
        );
    }

    #[tokio::test]
    async fn management_failure_is_not_misreported_as_recovered() {
        let mut platform = RecordingPlatform::new(InitBehavior::Fail);
        platform.management_fails = true;
        let mut publisher = RecordingRecoveryPublisher::for_platform(&platform);
        let error = initialize_or_recover(
            &mut platform,
            &test_identity(),
            &mut publisher,
            &ImmediateClock,
            Duration::from_secs(90),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("injected management failure"));
        assert_eq!(platform.snapshot().last(), Some(&"management-only"));
    }

    #[cfg(feature = "sim-hal")]
    mod sim_platform_recovery {
        use super::*;
        use dcentrald_asic::dspic::{DspicFirmware, DspicService};
        use dcentrald_hal::i2c::I2cServiceHandle;
        use dcentrald_hal::platform::sim::{SimModel, SimPlatform};
        use tokio_util::sync::CancellationToken;

        const DSPIC_ADDR: u8 = 0x20;

        fn sim_s19pro_identity() -> PlatformIdentitySnapshot {
            PlatformIdentitySnapshot {
                // This is declared simulator/package metadata, not a claim
                // that the simulated BM1398 was measured from real silicon.
                declared_board_target: Some("am2-s19pro".to_string()),
                board_desc: dcentrald_common::BoardDesc::lookup("am2-s19pro"),
                declared_platform_marker: Some("zynq-bm3-am2".to_string()),
                declared_subtype: None,
                declared_psu_hardware_variant: None,
                observed_control_board: "SimPlatform(Zynq)".to_string(),
            }
        }

        struct SimPlatformLifecycle {
            platform: SimPlatform,
            service: I2cServiceHandle,
            management_shutdown: CancellationToken,
            events: Arc<Mutex<Vec<&'static str>>>,
        }

        impl SimPlatformLifecycle {
            fn new() -> Result<Self> {
                let platform = SimPlatform::new(SimModel::S19Pro);
                let service = platform.open_i2c_service(0)?;
                let management_shutdown = CancellationToken::new();
                // Management mode normally parks until the process token is
                // cancelled. Pre-cancel it so the test executes that await and
                // returns deterministically after proving the handoff.
                management_shutdown.cancel();
                Ok(Self {
                    platform,
                    service,
                    management_shutdown,
                    events: Arc::new(Mutex::new(Vec::new())),
                })
            }

            fn record(&self, event: &'static str) {
                self.events.lock().unwrap().push(event);
            }

            fn snapshot(&self) -> Vec<&'static str> {
                self.events.lock().unwrap().clone()
            }
        }

        impl PlatformLifecycle for SimPlatformLifecycle {
            async fn initialize_platform(
                &mut self,
                identity: &PlatformIdentitySnapshot,
            ) -> Result<()> {
                assert_eq!(identity, &sim_s19pro_identity());
                self.record("initialize");
                // This is simulator-only state injection using the exact bare
                // dsPIC grammar. It does not alter or stand in for production
                // daemon wire selection.
                self.service
                    .write_bytes(DSPIC_ADDR, &[0x55, 0xAA, 0x15, 0x01])?;
                assert!(self.platform.i2c_voltage_enabled()?);
                std::future::pending().await
            }

            fn stop_initialization_keepalives(&mut self) {
                self.record("stop-keepalives");
            }

            async fn safe_off_partial_platform(&mut self) -> Result<()> {
                self.record("safe-off");
                self.service.latch_terminal_safe_off();
                // Use the production typed dsPIC safe-off operation. SafeOff
                // remains admitted after the terminal barrier; energizing and
                // unclassified mutations do not.
                let mut controller = DspicService::new_with_firmware(
                    self.service.clone(),
                    DSPIC_ADDR,
                    DspicFirmware::Fw89,
                );
                controller.disable_voltage()?;
                let enabled = self.platform.i2c_voltage_enabled()?;
                assert!(
                    !enabled,
                    "typed dsPIC safe-off did not cut simulated rail; trace={:?}",
                    self.platform.drain_i2c_trace()?
                );
                Ok(())
            }

            async fn run_management_only(&mut self) -> Result<()> {
                // The management handoff must not begin until SimPlatform has
                // observable rail-off state.
                assert!(!self.platform.i2c_voltage_enabled()?);
                self.record("management-only");
                self.management_shutdown.cancelled().await;
                Ok(())
            }
        }

        struct SimRecoveryPublisher {
            events: Arc<Mutex<Vec<&'static str>>>,
            fails: bool,
        }

        impl SimRecoveryPublisher {
            fn for_platform(platform: &SimPlatformLifecycle) -> Self {
                Self {
                    events: Arc::clone(&platform.events),
                    fails: false,
                }
            }
        }

        impl RecoveryPublisher for SimRecoveryPublisher {
            fn publish_management_recovery(&mut self) -> Result<()> {
                // This capability owns no SimPlatform hardware. Keeping the
                // publication seam separate proves a failed journal/store
                // cannot reorder or suppress rail safe-off.
                self.events.lock().unwrap().push("mark-recovery");
                if self.fails {
                    Err(anyhow!("injected simulator recovery publication failure"))
                } else {
                    Ok(())
                }
            }
        }

        #[tokio::test]
        async fn platform_identity_snapshot_survives_timed_out_sim_platform_recovery() {
            let mut platform = SimPlatformLifecycle::new().unwrap();
            let mut publisher = SimRecoveryPublisher::for_platform(&platform);
            let disposition = initialize_or_recover(
                &mut platform,
                &sim_s19pro_identity(),
                &mut publisher,
                &PollOnceTimeoutClock,
                Duration::from_secs(90),
            )
            .await
            .unwrap();

            assert_eq!(disposition, BringupDisposition::ManagementOnlyStopped);
            assert_eq!(
                platform.snapshot(),
                [
                    "initialize",
                    "stop-keepalives",
                    "safe-off",
                    "mark-recovery",
                    "management-only",
                ]
            );
            assert!(!platform.platform.i2c_voltage_enabled().unwrap());
            assert!(platform.service.terminal_safe_off_is_latched());
        }

        #[tokio::test]
        async fn failed_recovery_publication_cannot_suppress_simulated_rail_safe_off() {
            let mut platform = SimPlatformLifecycle::new().unwrap();
            let mut publisher = SimRecoveryPublisher::for_platform(&platform);
            publisher.fails = true;
            let disposition = initialize_or_recover(
                &mut platform,
                &sim_s19pro_identity(),
                &mut publisher,
                &PollOnceTimeoutClock,
                Duration::from_secs(90),
            )
            .await
            .unwrap();

            assert_eq!(disposition, BringupDisposition::ManagementOnlyStopped);
            assert_eq!(
                platform.snapshot(),
                [
                    "initialize",
                    "stop-keepalives",
                    "safe-off",
                    "mark-recovery",
                    "management-only",
                ]
            );
            assert!(!platform.platform.i2c_voltage_enabled().unwrap());
            assert!(platform.service.terminal_safe_off_is_latched());
        }
    }
}
