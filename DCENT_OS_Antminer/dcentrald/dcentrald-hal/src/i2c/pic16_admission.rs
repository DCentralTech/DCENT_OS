//! Worker-owned PIC16F1704 startup admission.
//!
//! Admission is a bus-level transaction, not a caller-paced series of I2C
//! calls. One service generation covers observation, bootloader transition,
//! qualification, activation, final proof, and admitted-batch handoff. The worker
//! advances at most one bounded wire operation per scheduler turn so reserved
//! SafeOff work remains preemptive during protocol settle intervals.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use super::{
    execute_disable_voltage, execute_heartbeat, execute_pic16_enable_only, execute_pic16_jump_only,
    execute_pic16_set_voltage_only, read_pic16_raw_exact_worker, HalError, I2cBus,
    I2cOperationIntent, I2cPicFirmware, I2cSafetyAuthority, I2cSafetyPermit, I2cServiceHandle,
    Result, PIC16_ADMISSION_ACTIVE_BIT, PIC16_ADMISSION_IDLE, PIC16_ADMISSION_TOKEN_MAX,
};

const PIC16_JUMP_SETTLE: Duration = Duration::from_millis(500);
const PIC16_HEARTBEAT_PERIOD: Duration = Duration::from_secs(1);
const PIC16_QUALIFICATION_ROUNDS: u8 = 5;
const PIC16_SET_ENABLE_GAP: Duration = Duration::from_millis(5);
const PIC16_ADMISSION_WAIT: Duration = Duration::from_secs(45);
const PIC16_ADOPTION_WAIT: Duration = Duration::from_secs(1);
const PIC16_FINALIZATION_WAIT: Duration = Duration::from_secs(15);

const PIC16_ADOPTION_OFFERED: u8 = 0;
const PIC16_ADOPTION_CALLER_READY: u8 = 1;
const PIC16_ADOPTION_COMMITTING: u8 = 2;
const PIC16_ADOPTION_COMMITTED: u8 = 3;
const PIC16_ADOPTION_ABORTED: u8 = 4;

/// Worker-observed evidence that a PIC16 endpoint reached application mode.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pic16ApplicationEvidence {
    /// Stock Bitmain application firmware version byte.
    Stock { version: u8 },
    /// Braiins OS application firmware version byte (`0x03`).
    BraiinsOs,
    /// Raw application-state marker (`0x60`) without a revision transaction.
    ApplicationModeUnknown,
}

/// Per-endpoint action performed after shared PIC16 liveness qualification.
///
/// A running handoff endpoint is never transitioned out of bootloader, written
/// with a new setpoint, or enabled. Callers may choose that mode only from
/// authoritative ASIC-live handoff evidence; controller application state by
/// itself does not prove that a hash-board rail is energized or correctly set.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pic16AdmissionMode {
    /// Continue a proven running endpoint without changing its power setup.
    ContinueProvenRunning,
    /// Program the clamped DAC and enable the endpoint after qualification.
    ProgramAndEnable { pic_value: u8 },
}

impl Pic16AdmissionMode {
    fn programmed_pic_value(self) -> Option<u8> {
        match self {
            Self::ContinueProvenRunning => None,
            Self::ProgramAndEnable { pic_value } => Some(pic_value),
        }
    }
}

/// One discovery-bound endpoint in an atomic mixed PIC16 admission batch.
///
/// The endpoint is intentionally consumed. A topology capability cannot be
/// reused to construct parallel admission owners after it enters a batch.
#[derive(Debug)]
pub struct Pic16AdmissionTarget {
    endpoint: crate::platform::VoltageControllerEndpoint,
    mode: Pic16AdmissionMode,
    running_fence: Option<Pic16RunningFence>,
}

/// Opaque proof that one discovery endpoint belongs to a running hash-board
/// handoff whose ASIC-side liveness was established independently.
///
/// There is deliberately no public constructor. Platform/ASIC handoff code
/// must mint this capability from its authoritative live-chain evidence; a
/// caller-provided address, model hint, or responsive PIC application byte is
/// insufficient.
///
/// The production build intentionally has no issuer yet. Until a HAL-owned
/// ASIC enumeration service can mint a live-chain lease, this capability is
/// available only through the explicit host simulator handoff API. This keeps
/// mixed hot/cold admission fail-closed instead of treating PIC responsiveness
/// as proof that ASICs are alive.
pub struct Pic16RunningEndpoint {
    endpoint: crate::platform::VoltageControllerEndpoint,
    service_bus: u8,
    generation: u64,
    authority: Weak<I2cSafetyAuthority>,
    live_chain_lease: Weak<AtomicBool>,
}

impl Pic16RunningEndpoint {
    pub fn bus(&self) -> u8 {
        self.endpoint.bus()
    }

    pub fn address(&self) -> u8 {
        self.endpoint.address()
    }

    #[cfg(feature = "sim-hal")]
    pub(crate) fn from_verified_handoff(
        endpoint: crate::platform::VoltageControllerEndpoint,
        service: &super::I2cServiceHandle,
        live_chain_lease: Weak<AtomicBool>,
    ) -> Result<Self> {
        super::validate_pic16_endpoint_capability(
            service.bus(),
            &endpoint,
            "running PIC16 handoff evidence",
        )?;
        if !live_chain_lease
            .upgrade()
            .is_some_and(|lease| lease.load(Ordering::SeqCst))
        {
            return Err(HalError::I2cSafetySuperseded {
                bus: service.bus(),
                addr: endpoint.address(),
                detail: "PIC16 running-handoff evidence was stale before capture".into(),
            });
        }
        let token = service.capture_generation_token()?;
        Ok(Self {
            endpoint,
            service_bus: token.bus,
            generation: token.generation,
            authority: token.authority,
            live_chain_lease,
        })
    }

    fn into_parts(
        self,
    ) -> (
        crate::platform::VoltageControllerEndpoint,
        Pic16RunningFence,
    ) {
        (
            self.endpoint,
            Pic16RunningFence {
                service_bus: self.service_bus,
                generation: self.generation,
                authority: self.authority,
                live_chain_lease: self.live_chain_lease,
            },
        )
    }
}

impl fmt::Debug for Pic16RunningEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pic16RunningEndpoint")
            .field("bus", &self.endpoint.bus())
            .field(
                "address",
                &format_args!("0x{:02X}", self.endpoint.address()),
            )
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

pub(super) struct Pic16RunningFence {
    service_bus: u8,
    generation: u64,
    authority: Weak<I2cSafetyAuthority>,
    live_chain_lease: Weak<AtomicBool>,
}

impl fmt::Debug for Pic16RunningFence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pic16RunningFence")
            .field("service_bus", &self.service_bus)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Pic16RunningFence {
    fn is_current(
        &self,
        service_bus: u8,
        service_authority: &Arc<I2cSafetyAuthority>,
        generation: u64,
    ) -> bool {
        self.service_bus == service_bus
            && self.generation == generation
            && self
                .authority
                .upgrade()
                .is_some_and(|authority| Arc::ptr_eq(&authority, service_authority))
            && service_authority.validate(I2cOperationIntent::KeepAlive, generation)
            && self
                .live_chain_lease
                .upgrade()
                .is_some_and(|lease| lease.load(Ordering::SeqCst))
    }
}

impl Pic16AdmissionTarget {
    pub fn program_and_enable(
        endpoint: crate::platform::VoltageControllerEndpoint,
        pic_value: u8,
    ) -> Self {
        let pic_value = super::clamp_pic_voltage_dac(pic_value);
        Self {
            endpoint,
            mode: Pic16AdmissionMode::ProgramAndEnable { pic_value },
            running_fence: None,
        }
    }

    /// Continue a controller using independently minted ASIC-live handoff
    /// evidence for the matching hash-board power domain.
    pub fn continue_proven_running(running: Pic16RunningEndpoint) -> Self {
        let (endpoint, running_fence) = running.into_parts();
        Self {
            endpoint,
            mode: Pic16AdmissionMode::ContinueProvenRunning,
            running_fence: Some(running_fence),
        }
    }

    pub fn mode(&self) -> Pic16AdmissionMode {
        self.mode
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        crate::platform::VoltageControllerEndpoint,
        Pic16AdmissionMode,
        Option<Pic16RunningFence>,
    ) {
        (self.endpoint, self.mode, self.running_fence)
    }
}

#[derive(Debug)]
pub(crate) struct Pic16AdmissionPlan {
    address: u8,
    mode: Pic16AdmissionMode,
    running_fence: Option<Pic16RunningFence>,
}

impl Pic16AdmissionPlan {
    pub(super) fn new(
        address: u8,
        mode: Pic16AdmissionMode,
        running_fence: Option<Pic16RunningFence>,
    ) -> Self {
        Self {
            address,
            mode,
            running_fence,
        }
    }

    pub(super) fn address(&self) -> u8 {
        self.address
    }
}

/// Stable classification of the admission boundary that failed.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pic16AdmissionStage {
    CallerWait,
    BatchAdoption,
    TransportReopen,
    Cancellation,
    GenerationFence,
    RunningEvidence,
    ApplicationObservation,
    BootloaderJump,
    Qualification,
    PostJumpObservation,
    QualificationHeartbeat,
    QualificationSchedule,
    SetVoltage,
    EnableVoltage,
    FinalHeartbeat,
    BatchPublication,
    Admission,
}

impl Pic16AdmissionStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CallerWait => "caller wait",
            Self::BatchAdoption => "batch adoption",
            Self::TransportReopen => "transport reopen",
            Self::Cancellation => "cancellation",
            Self::GenerationFence => "generation fence",
            Self::RunningEvidence => "running evidence",
            Self::ApplicationObservation => "application observation",
            Self::BootloaderJump => "bootloader JUMP",
            Self::Qualification => "qualification",
            Self::PostJumpObservation => "post-JUMP observation",
            Self::QualificationHeartbeat => "qualification heartbeat",
            Self::QualificationSchedule => "qualification schedule",
            Self::SetVoltage => "SET_VOLTAGE",
            Self::EnableVoltage => "ENABLE_VOLTAGE",
            Self::FinalHeartbeat => "final heartbeat",
            Self::BatchPublication => "batch publication",
            Self::Admission => "admission",
        }
    }
}

/// Opaque endpoint identity within one admitted PIC16 batch.
///
/// This value is cloneable for daemon routing tables, but it grants no wire
/// authority by itself. Runtime operations also require mutable access to the
/// exact non-cloneable [`Pic16AdmittedBatch`] that minted it.
#[derive(Clone)]
pub struct Pic16RuntimeEndpointId {
    ordinal: usize,
    batch: Weak<Pic16BatchAuthority>,
}

impl fmt::Debug for Pic16RuntimeEndpointId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pic16RuntimeEndpointId")
            .field("ordinal", &self.ordinal)
            .finish_non_exhaustive()
    }
}

/// Diagnostic metadata for one endpoint in an admitted batch.
#[derive(Debug)]
pub struct Pic16AdmittedEndpoint {
    id: Pic16RuntimeEndpointId,
    address: u8,
    evidence: Pic16ApplicationEvidence,
    mode: Pic16AdmissionMode,
}

impl Pic16AdmittedEndpoint {
    pub fn id(&self) -> Pic16RuntimeEndpointId {
        self.id.clone()
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn evidence(&self) -> Pic16ApplicationEvidence {
        self.evidence
    }

    pub fn mode(&self) -> Pic16AdmissionMode {
        self.mode
    }
}

/// Non-cloneable authority for one completely admitted PIC16 batch.
///
/// Endpoint metadata may be inspected and endpoint IDs may be retained for
/// routing, but all runtime wire operations require this exact batch owner.
/// This prevents fragmentable endpoint authority and cross-batch assembly.
#[must_use = "dropping an admitted PIC16 batch enqueues whole-batch SafeOff"]
pub struct Pic16AdmittedBatch {
    bus: u8,
    generation: u64,
    authority: Weak<I2cSafetyAuthority>,
    batch: Arc<Pic16BatchAuthority>,
    endpoints: Vec<Pic16AdmittedEndpoint>,
    drop_safe_off_service: Option<I2cServiceHandle>,
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

impl fmt::Debug for Pic16AdmittedBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pic16AdmittedBatch")
            .field("bus", &self.bus)
            .field("generation", &self.generation)
            .field("batch_epoch", &self.batch.epoch())
            .field("endpoints", &self.endpoints)
            .field("current", &self.is_current())
            .field("batch_released", &self.batch.released())
            .finish_non_exhaustive()
    }
}

impl Pic16AdmittedBatch {
    pub fn bus(&self) -> u8 {
        self.bus
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn batch_epoch(&self) -> u64 {
        self.batch.epoch()
    }

    pub fn endpoints(&self) -> &[Pic16AdmittedEndpoint] {
        &self.endpoints
    }

    /// Point-in-time diagnostic only. Every operation revalidates in-worker.
    pub fn is_current(&self) -> bool {
        self.authority.upgrade().is_some_and(|authority| {
            authority.validate(I2cOperationIntent::KeepAlive, self.generation)
                && authority
                    .active_pic16_batch()
                    .is_some_and(|active| Arc::ptr_eq(&active, &self.batch))
        }) && self.batch.runtime_liveness_is_current()
    }

    /// Derive cloneable, disable-only batch authority for independent teardown.
    pub fn safe_off_handle(&self) -> Pic16SafeOffHandle {
        Pic16SafeOffHandle {
            bus: self.bus,
            authority: Weak::clone(&self.authority),
            batch: Arc::clone(&self.batch),
        }
    }

    pub(super) fn authority(&self) -> Option<Arc<I2cSafetyAuthority>> {
        self.authority.upgrade()
    }

    pub(super) fn batch_for_worker(&self) -> Arc<Pic16BatchAuthority> {
        Arc::clone(&self.batch)
    }

    pub(super) fn arm_drop_safe_off(&mut self, service: I2cServiceHandle) {
        debug_assert!(self.drop_safe_off_service.is_none());
        self.drop_safe_off_service = Some(service);
    }

    pub(super) fn endpoint(&self, id: &Pic16RuntimeEndpointId) -> Option<&Pic16AdmittedEndpoint> {
        let id_batch = id.batch.upgrade()?;
        if !Arc::ptr_eq(&id_batch, &self.batch) {
            return None;
        }
        self.endpoints.get(id.ordinal)
    }
}

impl Drop for Pic16AdmittedBatch {
    fn drop(&mut self) {
        if self.batch.released() {
            return;
        }
        let Some(service) = self.drop_safe_off_service.take() else {
            // Provisional delivery remains worker-owned until adoption commits.
            return;
        };
        service.enqueue_pic16_batch_safe_off_on_drop(Arc::clone(&self.batch));
    }
}

/// Completed fixed-heartbeat frame for every endpoint in one admitted batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pic16HeartbeatRoundOutcome {
    batch_epoch: u64,
    generation: u64,
    addresses: Arc<[u8]>,
}

impl Pic16HeartbeatRoundOutcome {
    pub fn batch_epoch(&self) -> u64 {
        self.batch_epoch
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn addresses(&self) -> &[u8] {
        &self.addresses
    }

    pub(super) fn new(batch_epoch: u64, generation: u64, addresses: Arc<[u8]>) -> Self {
        Self {
            batch_epoch,
            generation,
            addresses,
        }
    }
}

/// Evidence that the canonical clamped SET_VOLTAGE frame completed.
///
/// This is wire-completion evidence, not a physical DAC or rail readback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pic16SetVoltageOutcome {
    batch_epoch: u64,
    address: u8,
    requested_pic_value: u8,
    canonical_pic_value: u8,
}

impl Pic16SetVoltageOutcome {
    pub fn batch_epoch(&self) -> u64 {
        self.batch_epoch
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn requested_pic_value(&self) -> u8 {
        self.requested_pic_value
    }

    pub fn canonical_pic_value(&self) -> u8 {
        self.canonical_pic_value
    }

    pub(super) fn new(
        batch_epoch: u64,
        address: u8,
        requested_pic_value: u8,
        canonical_pic_value: u8,
    ) -> Self {
        Self {
            batch_epoch,
            address,
            requested_pic_value,
            canonical_pic_value,
        }
    }
}

/// Cloneable authority that can only request monotonic SafeOff for one batch.
#[derive(Clone)]
pub struct Pic16SafeOffHandle {
    bus: u8,
    authority: Weak<I2cSafetyAuthority>,
    batch: Arc<Pic16BatchAuthority>,
}

impl fmt::Debug for Pic16SafeOffHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pic16SafeOffHandle")
            .field("bus", &self.bus)
            .field("addresses", &self.batch.addresses())
            .field("released", &self.batch.released())
            .finish_non_exhaustive()
    }
}

impl Pic16SafeOffHandle {
    pub(super) fn for_batch(
        bus: u8,
        authority: &Arc<I2cSafetyAuthority>,
        batch: Arc<Pic16BatchAuthority>,
    ) -> Self {
        Self {
            bus,
            authority: Arc::downgrade(authority),
            batch,
        }
    }

    pub fn bus(&self) -> u8 {
        self.bus
    }

    pub fn addresses(&self) -> &[u8] {
        self.batch.addresses()
    }

    pub fn batch_epoch(&self) -> u64 {
        self.batch.epoch()
    }

    pub fn is_released(&self) -> bool {
        self.batch.released()
    }

    pub(super) fn authority(&self) -> Option<Arc<I2cSafetyAuthority>> {
        self.authority.upgrade()
    }

    pub(super) fn batch(&self) -> Arc<Pic16BatchAuthority> {
        Arc::clone(&self.batch)
    }
}

#[derive(Debug)]
pub(crate) struct Pic16BatchAuthority {
    epoch: u64,
    addresses: Arc<[u8]>,
    /// Aggregate ASIC-side liveness for every ContinueProvenRunning endpoint.
    /// Installed exactly once immediately before provisional batch delivery.
    /// Runtime authority is invalid until the complete batch scope exists.
    runtime_liveness: OnceLock<Arc<[Pic16RuntimeLiveness]>>,
    released: AtomicBool,
    safe_off_ownership: Mutex<Pic16BatchSafeOffOwnership>,
    safe_off_finished: Condvar,
}

#[derive(Debug)]
struct Pic16RuntimeLiveness {
    address: u8,
    lease: Weak<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pic16BatchSafeOffOwnership {
    Idle,
    CallerClaimed,
    WorkerOwned,
}

pub(super) struct Pic16BatchSafeOffAttempt<'a> {
    batch: &'a Pic16BatchAuthority,
    handed_to_worker: bool,
}

impl Pic16BatchSafeOffAttempt<'_> {
    pub(super) fn handoff_to_worker(&mut self) {
        self.handed_to_worker = true;
        self.batch.mark_safe_off_worker_owned();
    }
}

impl Drop for Pic16BatchSafeOffAttempt<'_> {
    fn drop(&mut self) {
        if !self.handed_to_worker {
            self.batch.finish_safe_off_attempt();
        }
    }
}

impl Pic16BatchAuthority {
    pub(super) fn new(epoch: u64, addresses: Vec<u8>) -> Self {
        debug_assert_ne!(epoch, 0);
        Self {
            epoch,
            addresses: addresses.into(),
            runtime_liveness: OnceLock::new(),
            released: AtomicBool::new(false),
            safe_off_ownership: Mutex::new(Pic16BatchSafeOffOwnership::Idle),
            safe_off_finished: Condvar::new(),
        }
    }

    pub(super) fn addresses(&self) -> &[u8] {
        &self.addresses
    }

    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(super) fn released(&self) -> bool {
        self.released.load(Ordering::SeqCst)
    }

    pub(super) fn install_runtime_liveness(
        &self,
        leases: Vec<(u8, Weak<AtomicBool>)>,
    ) -> std::result::Result<(), &'static str> {
        if leases
            .iter()
            .any(|(address, _)| !self.addresses.contains(address))
        {
            return Err("runtime liveness contains an endpoint outside the batch");
        }
        if leases.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err("runtime liveness endpoints are not unique deterministic order");
        }
        self.runtime_liveness
            .set(
                leases
                    .into_iter()
                    .map(|(address, lease)| Pic16RuntimeLiveness { address, lease })
                    .collect::<Vec<_>>()
                    .into(),
            )
            .map_err(|_| "runtime liveness scope was already installed")
    }

    pub(super) fn runtime_liveness_is_current(&self) -> bool {
        !self.released()
            && self.runtime_liveness.get().is_some_and(|leases| {
                leases.iter().all(|endpoint| {
                    endpoint
                        .lease
                        .upgrade()
                        .is_some_and(|lease| lease.load(Ordering::SeqCst))
                })
            })
    }

    pub(super) fn expired_runtime_liveness_address(&self) -> Option<u8> {
        self.runtime_liveness.get().and_then(|leases| {
            leases.iter().find_map(|endpoint| {
                (!endpoint
                    .lease
                    .upgrade()
                    .is_some_and(|lease| lease.load(Ordering::SeqCst)))
                .then_some(endpoint.address)
            })
        })
    }

    pub(super) fn mark_released(&self) {
        self.released.store(true, Ordering::SeqCst);
    }

    pub(super) fn claim_safe_off_attempt(&self) -> Option<Pic16BatchSafeOffAttempt<'_>> {
        let mut ownership = self
            .safe_off_ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *ownership != Pic16BatchSafeOffOwnership::Idle {
            None
        } else {
            *ownership = Pic16BatchSafeOffOwnership::CallerClaimed;
            Some(Pic16BatchSafeOffAttempt {
                batch: self,
                handed_to_worker: false,
            })
        }
    }

    pub(super) fn claim_worker_safe_off_attempt(&self) -> bool {
        let mut ownership = self
            .safe_off_ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *ownership != Pic16BatchSafeOffOwnership::Idle {
            false
        } else {
            *ownership = Pic16BatchSafeOffOwnership::WorkerOwned;
            true
        }
    }

    fn mark_safe_off_worker_owned(&self) {
        let mut ownership = self
            .safe_off_ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert_eq!(*ownership, Pic16BatchSafeOffOwnership::CallerClaimed);
        *ownership = Pic16BatchSafeOffOwnership::WorkerOwned;
    }

    pub(super) fn finish_safe_off_attempt(&self) {
        let mut ownership = self
            .safe_off_ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *ownership = Pic16BatchSafeOffOwnership::Idle;
        self.safe_off_finished.notify_all();
    }

    pub(super) fn wait_for_safe_off_attempt(&self, budget: Duration) -> Pic16BatchSafeOffOwnership {
        let ownership = self
            .safe_off_ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *ownership == Pic16BatchSafeOffOwnership::Idle {
            return Pic16BatchSafeOffOwnership::Idle;
        }
        let (ownership, _) = self
            .safe_off_finished
            .wait_timeout_while(ownership, budget, |ownership| {
                *ownership != Pic16BatchSafeOffOwnership::Idle
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *ownership
    }
}

/// Successful atomic handoff for every endpoint in the requested batch.
///
/// Discovery does not prove a rail is off, so an endpoint that cannot qualify
/// fails the whole batch and triggers SafeOff for every requested address.
/// Partial success requires a future typed per-endpoint proven-off capability.
/// Worker-observed result of one best-effort rollback leg.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pic16CompensationStatus {
    Disabled,
    OutcomeUnknown { detail: String },
}

/// Per-endpoint rollback evidence retained alongside the initiating failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pic16CompensationOutcome {
    address: u8,
    status: Pic16CompensationStatus,
}

/// Deterministic result of disabling every endpoint in one admitted batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pic16BatchSafeOffOutcome {
    epoch: u64,
    disposition: Pic16BatchSafeOffDisposition,
    endpoints: Vec<Pic16CompensationOutcome>,
}

/// Meaning of a batch SafeOff result at its service-epoch boundary.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pic16BatchSafeOffDisposition {
    /// This call executed the worker-owned batch disable operation.
    Executed,
    /// This exact epoch had already completed a proven SafeOff and no newer
    /// PIC16 batch was active when the historical result was requested.
    AlreadyReleased,
}

impl Pic16BatchSafeOffOutcome {
    pub fn batch_epoch(&self) -> u64 {
        self.epoch
    }

    pub fn disposition(&self) -> Pic16BatchSafeOffDisposition {
        self.disposition
    }

    pub fn endpoints(&self) -> &[Pic16CompensationOutcome] {
        &self.endpoints
    }

    pub fn all_disabled(&self) -> bool {
        self.disposition == Pic16BatchSafeOffDisposition::Executed
            && self
                .endpoints
                .iter()
                .all(|outcome| outcome.status == Pic16CompensationStatus::Disabled)
    }

    pub(super) fn disabled(epoch: u64, endpoints: Vec<Pic16CompensationOutcome>) -> Self {
        Self {
            epoch,
            disposition: Pic16BatchSafeOffDisposition::Executed,
            endpoints,
        }
    }

    pub(super) fn already_released(epoch: u64) -> Self {
        Self {
            epoch,
            disposition: Pic16BatchSafeOffDisposition::AlreadyReleased,
            endpoints: Vec::new(),
        }
    }
}

impl Pic16CompensationOutcome {
    pub(super) fn new(address: u8, status: Pic16CompensationStatus) -> Self {
        Self { address, status }
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn status(&self) -> &Pic16CompensationStatus {
        &self.status
    }
}

/// Typed failure for a worker-owned PIC16 admission transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pic16AdmissionFailure {
    bus: u8,
    address: Option<u8>,
    stage: Pic16AdmissionStage,
    detail: String,
    compensation: Vec<Pic16CompensationOutcome>,
    cleanup_pending: bool,
}

impl Pic16AdmissionFailure {
    pub fn bus(&self) -> u8 {
        self.bus
    }

    pub fn address(&self) -> Option<u8> {
        self.address
    }

    pub fn stage(&self) -> Pic16AdmissionStage {
        self.stage
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn compensation(&self) -> &[Pic16CompensationOutcome] {
        &self.compensation
    }

    /// True when the caller's bounded wait ended before the worker could
    /// deliver final cleanup evidence. The worker still owns the cleanup.
    pub fn cleanup_pending(&self) -> bool {
        self.cleanup_pending
    }
}

impl fmt::Display for Pic16AdmissionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PIC16 admission failed on bus {} during {}",
            self.bus,
            self.stage.as_str()
        )?;
        if let Some(address) = self.address {
            write!(formatter, " at 0x{address:02X}")?;
        }
        write!(formatter, ": {}", self.detail)?;
        if !self.compensation.is_empty() {
            write!(
                formatter,
                " ({} compensation outcome(s))",
                self.compensation.len()
            )?;
        }
        if self.cleanup_pending {
            write!(formatter, " (cleanup remains worker-owned)")?;
        }
        Ok(())
    }
}

impl std::error::Error for Pic16AdmissionFailure {}

/// Cancel-on-drop owner for one admitted worker job.
pub struct Pic16AdmissionJob {
    bus: u8,
    cancellation: Arc<AtomicBool>,
    completion_rx: mpsc::Receiver<Pic16AdmissionDelivery>,
    drop_safe_off_service: Option<I2cServiceHandle>,
    finished: bool,
}

impl fmt::Debug for Pic16AdmissionJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pic16AdmissionJob")
            .field("bus", &self.bus)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Pic16AdmissionJob {
    pub(super) fn new(
        bus: u8,
        cancellation: Arc<AtomicBool>,
        completion_rx: mpsc::Receiver<Pic16AdmissionDelivery>,
        drop_safe_off_service: I2cServiceHandle,
    ) -> Self {
        Self {
            bus,
            cancellation,
            completion_rx,
            drop_safe_off_service: Some(drop_safe_off_service),
            finished: false,
        }
    }

    fn finish_success(&mut self, mut admitted: Pic16AdmittedBatch) -> Pic16AdmittedBatch {
        admitted.arm_drop_safe_off(
            self.drop_safe_off_service
                .take()
                .expect("adopted PIC16 batch has one drop-time SafeOff service"),
        );
        self.finished = true;
        admitted
    }

    /// Wait for atomic batch adoption. Timeout/drop requests worker cleanup.
    pub fn wait(mut self) -> std::result::Result<Pic16AdmittedBatch, Pic16AdmissionFailure> {
        let delivery = match self.completion_rx.recv_timeout(PIC16_ADMISSION_WAIT) {
            Ok(delivery) => delivery,
            Err(error) => {
                self.cancellation.store(true, Ordering::SeqCst);
                if let Ok(delivery) = self.completion_rx.recv_timeout(PIC16_FINALIZATION_WAIT) {
                    if let Err(failure) = delivery.result {
                        self.finished = true;
                        return Err(failure);
                    }
                    if let Some(adoption) = delivery.adoption {
                        let _ = adoption.state.compare_exchange(
                            PIC16_ADOPTION_OFFERED,
                            PIC16_ADOPTION_ABORTED,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        );
                        if let Ok(Err(failure)) =
                            adoption.final_rx.recv_timeout(PIC16_FINALIZATION_WAIT)
                        {
                            self.finished = true;
                            return Err(failure);
                        }
                    }
                }
                self.finished = true;
                return Err(Pic16AdmissionFailure {
                    bus: self.bus,
                    address: None,
                    stage: Pic16AdmissionStage::CallerWait,
                    detail: format!("worker completion was not received: {error}"),
                    compensation: Vec::new(),
                    cleanup_pending: true,
                });
            }
        };

        let outcome = match delivery.result {
            Ok(outcome) => outcome,
            Err(failure) => {
                self.finished = true;
                return Err(failure);
            }
        };
        let Some(adoption) = delivery.adoption else {
            self.cancellation.store(true, Ordering::SeqCst);
            self.finished = true;
            return Err(Pic16AdmissionFailure {
                bus: self.bus,
                address: None,
                stage: Pic16AdmissionStage::BatchAdoption,
                detail: "worker omitted the batch-adoption handshake".into(),
                compensation: Vec::new(),
                cleanup_pending: true,
            });
        };
        if adoption
            .state
            .compare_exchange(
                PIC16_ADOPTION_OFFERED,
                PIC16_ADOPTION_CALLER_READY,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            self.cancellation.store(true, Ordering::SeqCst);
            self.finished = true;
            return Err(Pic16AdmissionFailure {
                bus: self.bus,
                address: None,
                stage: Pic16AdmissionStage::BatchAdoption,
                detail: "batch adoption offer was no longer pending".into(),
                compensation: Vec::new(),
                cleanup_pending: true,
            });
        }
        match adoption.final_rx.recv_timeout(PIC16_FINALIZATION_WAIT) {
            Ok(Ok(())) => Ok(self.finish_success(outcome)),
            Ok(Err(failure)) => {
                self.finished = true;
                Err(failure)
            }
            Err(error) => {
                match adoption.state.compare_exchange(
                    PIC16_ADOPTION_CALLER_READY,
                    PIC16_ADOPTION_ABORTED,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => self.cancellation.store(true, Ordering::SeqCst),
                    Err(PIC16_ADOPTION_COMMITTED) => {
                        return Ok(self.finish_success(outcome));
                    }
                    Err(PIC16_ADOPTION_COMMITTING) => {}
                    Err(_) => self.cancellation.store(true, Ordering::SeqCst),
                }
                match adoption.final_rx.recv_timeout(PIC16_FINALIZATION_WAIT) {
                    Ok(Ok(()))
                        if adoption.state.load(Ordering::SeqCst) == PIC16_ADOPTION_COMMITTED =>
                    {
                        return Ok(self.finish_success(outcome));
                    }
                    Ok(Err(failure)) => {
                        self.finished = true;
                        return Err(failure);
                    }
                    Err(second_error) => {
                        let final_state = adoption.state.load(Ordering::SeqCst);
                        let irrevocably_adopted = final_state == PIC16_ADOPTION_COMMITTED
                            || (final_state == PIC16_ADOPTION_COMMITTING
                                && matches!(second_error, mpsc::RecvTimeoutError::Timeout));
                        if irrevocably_adopted {
                            // COMMITTING means the worker won the only CAS
                            // from CALLER_READY and can no longer compensate.
                            // A scheduling stall must therefore return the
                            // batch authority instead of discarding it as a
                            // cleanup failure. A disconnected sender while
                            // merely COMMITTING still fails closed because it
                            // can indicate worker unwind before commit.
                            return Ok(self.finish_success(outcome));
                        }
                    }
                    Ok(Ok(())) => {}
                }
                self.finished = true;
                Err(Pic16AdmissionFailure {
                    bus: self.bus,
                    address: None,
                    stage: Pic16AdmissionStage::BatchAdoption,
                    detail: format!("worker did not confirm batch adoption: {error}"),
                    compensation: Vec::new(),
                    cleanup_pending: true,
                })
            }
        }
    }
}

impl Drop for Pic16AdmissionJob {
    fn drop(&mut self) {
        if !self.finished {
            self.cancellation.store(true, Ordering::SeqCst);
        }
    }
}

/// Internal two-phase delivery between the worker and admission owner.
pub(crate) struct Pic16AdmissionDelivery {
    pub(super) result: std::result::Result<Pic16AdmittedBatch, Pic16AdmissionFailure>,
    pub(super) adoption: Option<Pic16AdmissionAdoption>,
}

/// Queue-lifetime guard closing reservation leaks on expiry/disconnect.
pub(crate) struct Pic16AdmissionReservation {
    authority: Arc<I2cSafetyAuthority>,
    token: u64,
    armed: bool,
}

impl fmt::Debug for Pic16AdmissionReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pic16AdmissionReservation")
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

impl Pic16AdmissionReservation {
    pub(super) fn reserve(
        authority: Arc<I2cSafetyAuthority>,
        bus: u8,
        address: u8,
    ) -> Result<Self> {
        // Whole-fabric recovery holds this lock from its final unmanaged
        // recheck through controller mutation. Taking it before reservation
        // publication makes those operations linearizable: admission cannot
        // become owned halfway through a recovery that already won the lock.
        let service_state = authority
            .pic16_service_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = authority
            .pic16_admission_sequence
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                (current < PIC16_ADMISSION_TOKEN_MAX).then_some(current + 1)
            })
            .map_err(|_| HalError::I2cAdmissionBusy {
                bus,
                addr: address,
                detail: "PIC16 admission reservation token space is exhausted".into(),
            })?;
        let token = previous + 1;
        authority
            .pic16_admission_owner
            .compare_exchange(
                PIC16_ADMISSION_IDLE,
                token,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .map_err(|_| super::hal_busy_error(bus, address))?;
        drop(service_state);
        Ok(Self {
            authority,
            token,
            armed: true,
        })
    }

    pub(super) fn token(&self) -> u64 {
        self.token
    }

    pub(super) fn revoke(authority: &I2cSafetyAuthority, token: u64) {
        let _ = authority.pic16_admission_owner.compare_exchange(
            token,
            PIC16_ADMISSION_IDLE,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub(super) fn activate(mut self) -> Result<u64> {
        self.authority
            .pic16_admission_owner
            .compare_exchange(
                self.token,
                self.token | PIC16_ADMISSION_ACTIVE_BIT,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .map_err(|observed| HalError::I2cAdmissionBusy {
                bus: 0,
                addr: 0,
                detail: format!(
                    "PIC16 admission reservation token {} could not activate from owner word {observed}",
                    self.token
                ),
            })?;
        self.armed = false;
        Ok(self.token)
    }
}

impl Drop for Pic16AdmissionReservation {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.authority.pic16_admission_owner.compare_exchange(
                self.token,
                PIC16_ADMISSION_IDLE,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
        }
    }
}

pub(super) struct Pic16AdmissionAdoption {
    state: Arc<std::sync::atomic::AtomicU8>,
    final_rx: mpsc::Receiver<std::result::Result<(), Pic16AdmissionFailure>>,
}

#[derive(Debug)]
struct EndpointState {
    address: u8,
    mode: Pic16AdmissionMode,
    running_fence: Option<Pic16RunningFence>,
    evidence: Option<Pic16ApplicationEvidence>,
    jumped: bool,
    application_ready_at: Option<Instant>,
    enable_not_before: Option<Instant>,
}

impl EndpointState {
    fn active(&self) -> bool {
        self.evidence.is_some()
    }

    fn programmed_pic_value(&self) -> Option<u8> {
        self.mode.programmed_pic_value()
    }

    fn running_fence_is_current(
        &self,
        bus: u8,
        authority: &Arc<I2cSafetyAuthority>,
        generation: u64,
    ) -> bool {
        match (&self.mode, &self.running_fence) {
            (Pic16AdmissionMode::ContinueProvenRunning, Some(fence)) => {
                fence.is_current(bus, authority, generation)
            }
            (Pic16AdmissionMode::ProgramAndEnable { .. }, None) => true,
            _ => false,
        }
    }
}

#[derive(Debug)]
enum AdmissionPhase {
    ObserveRead(usize),
    ObserveJump(usize),
    SettleRead(usize),
    Heartbeat {
        round: u8,
        index: usize,
    },
    SetVoltage(usize),
    Enable(usize),
    FinalHeartbeat(usize),
    Deliver,
    AwaitAdoption {
        state: Arc<std::sync::atomic::AtomicU8>,
        final_tx: mpsc::SyncSender<std::result::Result<(), Pic16AdmissionFailure>>,
        deadline: Instant,
    },
    Compensate {
        index: usize,
        retry: bool,
    },
    DeliverFailure,
    Finished,
}

pub(super) struct Pic16AdmissionState {
    bus: u8,
    permit: I2cSafetyPermit,
    cancellation: Arc<AtomicBool>,
    completion_tx: Option<mpsc::SyncSender<Pic16AdmissionDelivery>>,
    endpoints: Vec<EndpointState>,
    phase: AdmissionPhase,
    heartbeat_epoch: Option<Instant>,
    /// Every requested endpoint is conservatively shutdown-owned because
    /// discovery proves identity, not a de-energized rail.
    safe_off_order: Vec<u8>,
    compensation_permit: Option<I2cSafetyPermit>,
    compensation: Vec<Pic16CompensationOutcome>,
    primary_failure: Option<Pic16AdmissionFailure>,
    adoption_confirmation_tx:
        Option<mpsc::SyncSender<std::result::Result<(), Pic16AdmissionFailure>>>,
    reservation_token: u64,
    active_state_armed: bool,
    published_batch: Option<Arc<Pic16BatchAuthority>>,
}

pub(super) struct Pic16AdmissionStep {
    pub(super) next_due: Option<Instant>,
    pub(super) transport_fault: bool,
    pub(super) finished: bool,
}

impl Pic16AdmissionStep {
    fn ready() -> Self {
        Self {
            next_due: None,
            transport_fault: false,
            finished: false,
        }
    }

    fn wait_until(next_due: Instant) -> Self {
        Self {
            next_due: Some(next_due),
            ..Self::ready()
        }
    }
}

impl Pic16AdmissionState {
    pub(super) fn new(
        bus: u8,
        permit: I2cSafetyPermit,
        cancellation: Arc<AtomicBool>,
        completion_tx: mpsc::SyncSender<Pic16AdmissionDelivery>,
        plans: Vec<Pic16AdmissionPlan>,
        batch: Arc<Pic16BatchAuthority>,
        reservation_token: u64,
    ) -> Self {
        let safe_off_order = plans.iter().map(|plan| plan.address).collect();
        Self {
            bus,
            permit,
            cancellation,
            completion_tx: Some(completion_tx),
            endpoints: plans
                .into_iter()
                .map(|plan| EndpointState {
                    address: plan.address,
                    mode: plan.mode,
                    running_fence: plan.running_fence,
                    evidence: None,
                    jumped: false,
                    application_ready_at: None,
                    enable_not_before: None,
                })
                .collect(),
            phase: AdmissionPhase::ObserveRead(0),
            heartbeat_epoch: None,
            safe_off_order,
            compensation_permit: None,
            compensation: Vec::new(),
            primary_failure: None,
            adoption_confirmation_tx: None,
            reservation_token,
            active_state_armed: true,
            published_batch: Some(batch),
        }
    }

    pub(super) fn request_cancel(&self) {
        self.cancellation.store(true, Ordering::SeqCst);
    }

    /// Terminate when the worker cannot obtain any transport with which to
    /// continue or compensate. Attempted rails remain explicitly unknown and
    /// terminal mutation admission is latched; the hardware watchdog is then
    /// the independent cutoff mechanism.
    pub(super) fn transport_unavailable(&mut self, detail: impl Into<String>) {
        if self.primary_failure.is_none() {
            self.primary_failure = Some(Pic16AdmissionFailure {
                bus: self.bus,
                address: None,
                stage: Pic16AdmissionStage::TransportReopen,
                detail: detail.into(),
                compensation: Vec::new(),
                cleanup_pending: false,
            });
        }
        for &address in self.safe_off_order.iter().rev() {
            if !self
                .compensation
                .iter()
                .any(|outcome| outcome.address == address)
            {
                self.compensation.push(Pic16CompensationOutcome {
                    address,
                    status: Pic16CompensationStatus::OutcomeUnknown {
                        detail: "I2C transport unavailable for DISABLE compensation".into(),
                    },
                });
            }
        }
        if !self.safe_off_order.is_empty() {
            self.permit.authority.mark_pic16_shutdown_unresolved();
        }
        self.phase = AdmissionPhase::DeliverFailure;
    }

    pub(super) fn next_due(&self) -> Option<Instant> {
        match self.phase {
            AdmissionPhase::SettleRead(index) => self
                .endpoints
                .iter()
                .skip(index)
                .filter(|endpoint| endpoint.jumped)
                .filter_map(|endpoint| endpoint.application_ready_at)
                .min(),
            AdmissionPhase::Heartbeat { round, .. } => self
                .heartbeat_epoch
                .map(|epoch| epoch + PIC16_HEARTBEAT_PERIOD * u32::from(round)),
            AdmissionPhase::Enable(index) => self
                .endpoints
                .iter()
                .skip(index)
                .filter(|endpoint| endpoint.active())
                .filter_map(|endpoint| endpoint.enable_not_before)
                .min(),
            AdmissionPhase::AwaitAdoption { deadline, .. } => Some(deadline),
            _ => None,
        }
    }

    pub(super) fn requires_bus(&self) -> bool {
        !matches!(
            self.phase,
            AdmissionPhase::Deliver
                | AdmissionPhase::AwaitAdoption { .. }
                | AdmissionPhase::Compensate { index: 0, .. }
                | AdmissionPhase::DeliverFailure
                | AdmissionPhase::Finished
        )
    }

    pub(super) fn advance_without_bus(&mut self, now: Instant) -> Pic16AdmissionStep {
        debug_assert!(!self.requires_bus());
        self.apply_cancellation_fence();
        let phase = std::mem::replace(&mut self.phase, AdmissionPhase::Finished);
        match phase {
            AdmissionPhase::Deliver => self.deliver_success(now),
            AdmissionPhase::AwaitAdoption {
                state,
                final_tx,
                deadline,
            } => self.await_adoption(state, final_tx, deadline, now),
            AdmissionPhase::Compensate { index: 0, .. } => self.complete_compensation(),
            AdmissionPhase::DeliverFailure => self.deliver_failure(),
            AdmissionPhase::Finished => self.phase = AdmissionPhase::Finished,
            unexpected => {
                self.phase = unexpected;
                debug_assert!(false, "busless advance requested for a wire phase");
            }
        }
        Pic16AdmissionStep {
            next_due: self.next_due(),
            transport_fault: false,
            finished: matches!(self.phase, AdmissionPhase::Finished),
        }
    }

    pub(super) fn advance(&mut self, i2c: &mut I2cBus, now: Instant) -> Pic16AdmissionStep {
        self.apply_cancellation_fence();

        let phase = std::mem::replace(&mut self.phase, AdmissionPhase::Finished);
        let mut step = Pic16AdmissionStep::ready();
        match phase {
            AdmissionPhase::ObserveRead(index) => self.observe_read(i2c, index, &mut step),
            AdmissionPhase::ObserveJump(index) => self.observe_jump(i2c, index, now, &mut step),
            AdmissionPhase::SettleRead(index) => self.settle_read(i2c, index, now, &mut step),
            AdmissionPhase::Heartbeat { round, index } => {
                self.qualification_heartbeat(i2c, round, index, now, &mut step)
            }
            AdmissionPhase::SetVoltage(index) => self.set_voltage(i2c, index, now, &mut step),
            AdmissionPhase::Enable(index) => self.enable(i2c, index, now, &mut step),
            AdmissionPhase::FinalHeartbeat(index) => self.final_heartbeat(i2c, index, &mut step),
            AdmissionPhase::Deliver => self.deliver_success(now),
            AdmissionPhase::AwaitAdoption {
                state,
                final_tx,
                deadline,
            } => self.await_adoption(state, final_tx, deadline, now),
            AdmissionPhase::Compensate { index, retry } => {
                self.compensate(i2c, index, retry, &mut step)
            }
            AdmissionPhase::DeliverFailure => self.deliver_failure(),
            AdmissionPhase::Finished => self.phase = AdmissionPhase::Finished,
        }
        step.next_due = step.next_due.or_else(|| self.next_due());
        step.finished = matches!(self.phase, AdmissionPhase::Finished);
        step
    }

    fn observe_read(&mut self, i2c: &mut I2cBus, index: usize, step: &mut Pic16AdmissionStep) {
        if index >= self.endpoints.len() {
            self.phase = AdmissionPhase::SettleRead(0);
            return;
        }
        let address = self.endpoints[index].address;
        match read_pic16_raw_exact_worker(
            i2c,
            address,
            &self.permit,
            "PIC16 admission exact boot-state read",
        ) {
            Ok(0xCC)
                if matches!(
                    self.endpoints[index].mode,
                    Pic16AdmissionMode::ProgramAndEnable { .. }
                ) =>
            {
                self.phase = AdmissionPhase::ObserveJump(index)
            }
            Ok(0xCC) => {
                self.fail(
                    Some(address),
                    Pic16AdmissionStage::ApplicationObservation,
                    "proven-running endpoint was observed in exact bootloader state 0xCC",
                );
            }
            Ok(raw) => {
                if let Some(evidence) = classify_application(raw) {
                    self.endpoints[index].evidence = Some(evidence);
                } else {
                    self.fail(
                        Some(address),
                        Pic16AdmissionStage::ApplicationObservation,
                        format!("unsupported raw state 0x{raw:02X}"),
                    );
                    return;
                }
                self.phase = AdmissionPhase::ObserveRead(index + 1);
            }
            Err(error) => self.pre_activation_error(
                index,
                Pic16AdmissionStage::ApplicationObservation,
                error,
                step,
            ),
        }
    }

    fn observe_jump(
        &mut self,
        i2c: &mut I2cBus,
        index: usize,
        now: Instant,
        step: &mut Pic16AdmissionStep,
    ) {
        let address = self.endpoints[index].address;
        let host_start = Instant::now();
        let service_start = i2c.service_time();
        match execute_pic16_jump_only(i2c, address, &self.permit) {
            Ok(()) => {
                let completed_at = i2c.scheduled_completion_time(now, host_start, service_start);
                self.endpoints[index].jumped = true;
                self.endpoints[index].application_ready_at = Some(completed_at + PIC16_JUMP_SETTLE);
                self.phase = AdmissionPhase::ObserveRead(index + 1);
            }
            Err(error) => {
                self.pre_activation_error(index, Pic16AdmissionStage::BootloaderJump, error, step)
            }
        }
    }

    fn settle_read(
        &mut self,
        i2c: &mut I2cBus,
        mut index: usize,
        now: Instant,
        step: &mut Pic16AdmissionStep,
    ) {
        while index < self.endpoints.len() && !self.endpoints[index].jumped {
            index += 1;
        }
        if index >= self.endpoints.len() {
            if !self.any_active() {
                self.fail(
                    None,
                    Pic16AdmissionStage::Qualification,
                    "no PIC16 endpoint reached application mode",
                );
            } else {
                self.heartbeat_epoch = Some(now);
                self.phase = AdmissionPhase::Heartbeat { round: 0, index: 0 };
            }
            return;
        }
        let due = self.endpoints[index]
            .application_ready_at
            .expect("jumped endpoint has a settle deadline");
        if now < due {
            self.phase = AdmissionPhase::SettleRead(index);
            *step = Pic16AdmissionStep::wait_until(due);
            return;
        }
        let address = self.endpoints[index].address;
        match read_pic16_raw_exact_worker(
            i2c,
            address,
            &self.permit,
            "PIC16 admission exact post-JUMP read",
        ) {
            Ok(raw) => {
                if let Some(evidence) = classify_application(raw) {
                    self.endpoints[index].evidence = Some(evidence);
                } else {
                    self.fail(
                        Some(address),
                        Pic16AdmissionStage::PostJumpObservation,
                        format!("unsupported raw state 0x{raw:02X}"),
                    );
                    return;
                }
                self.phase = AdmissionPhase::SettleRead(index + 1);
            }
            Err(error) => self.pre_activation_error(
                index,
                Pic16AdmissionStage::PostJumpObservation,
                error,
                step,
            ),
        }
    }

    fn qualification_heartbeat(
        &mut self,
        i2c: &mut I2cBus,
        round: u8,
        mut index: usize,
        now: Instant,
        step: &mut Pic16AdmissionStep,
    ) {
        let due = self.heartbeat_epoch.expect("heartbeat epoch is set")
            + PIC16_HEARTBEAT_PERIOD * u32::from(round);
        if round > 0 && now >= due + PIC16_HEARTBEAT_PERIOD {
            self.fail(
                None,
                Pic16AdmissionStage::QualificationSchedule,
                format!(
                    "heartbeat round {} missed its anchored one-second window",
                    round + 1
                ),
            );
            return;
        }
        if now < due {
            self.phase = AdmissionPhase::Heartbeat { round, index };
            *step = Pic16AdmissionStep::wait_until(due);
            return;
        }
        while index < self.endpoints.len() && !self.endpoints[index].active() {
            index += 1;
        }
        if index >= self.endpoints.len() {
            if !self.any_active() {
                self.fail(
                    None,
                    Pic16AdmissionStage::QualificationHeartbeat,
                    "every PIC16 endpoint was rejected",
                );
            } else if round + 1 < PIC16_QUALIFICATION_ROUNDS {
                self.phase = AdmissionPhase::Heartbeat {
                    round: round + 1,
                    index: 0,
                };
            } else {
                self.phase = AdmissionPhase::SetVoltage(0);
            }
            return;
        }
        let address = self.endpoints[index].address;
        match execute_heartbeat(i2c, address, I2cPicFirmware::Unknown, &self.permit) {
            Ok(()) => {
                self.phase = AdmissionPhase::Heartbeat {
                    round,
                    index: index + 1,
                }
            }
            Err(error) => self.pre_activation_error(
                index,
                Pic16AdmissionStage::QualificationHeartbeat,
                error,
                step,
            ),
        }
    }

    fn set_voltage(
        &mut self,
        i2c: &mut I2cBus,
        mut index: usize,
        now: Instant,
        step: &mut Pic16AdmissionStep,
    ) {
        while index < self.endpoints.len()
            && (!self.endpoints[index].active()
                || self.endpoints[index].programmed_pic_value().is_none())
        {
            index += 1;
        }
        if index >= self.endpoints.len() {
            self.phase = AdmissionPhase::Enable(0);
            return;
        }
        let address = self.endpoints[index].address;
        let pic_value = self.endpoints[index]
            .programmed_pic_value()
            .expect("programmed endpoint has a PIC value");
        let host_start = Instant::now();
        let service_start = i2c.service_time();
        match execute_pic16_set_voltage_only(i2c, address, pic_value, &self.permit) {
            Ok(()) => {
                let completed_at = i2c.scheduled_completion_time(now, host_start, service_start);
                self.endpoints[index].enable_not_before = Some(completed_at + PIC16_SET_ENABLE_GAP);
                self.phase = AdmissionPhase::SetVoltage(index + 1);
            }
            Err(error) => {
                step.transport_fault = is_transport_fault(&error);
                self.fail(
                    Some(address),
                    Pic16AdmissionStage::SetVoltage,
                    error.to_string(),
                );
            }
        }
    }

    fn enable(
        &mut self,
        i2c: &mut I2cBus,
        mut index: usize,
        now: Instant,
        step: &mut Pic16AdmissionStep,
    ) {
        while index < self.endpoints.len()
            && (!self.endpoints[index].active()
                || self.endpoints[index].programmed_pic_value().is_none())
        {
            index += 1;
        }
        if index >= self.endpoints.len() {
            self.phase = AdmissionPhase::FinalHeartbeat(0);
            return;
        }
        let due = self.endpoints[index]
            .enable_not_before
            .expect("SET endpoint has an enable deadline");
        if now < due {
            self.phase = AdmissionPhase::Enable(index);
            *step = Pic16AdmissionStep::wait_until(due);
            return;
        }
        let address = self.endpoints[index].address;
        match execute_pic16_enable_only(i2c, address, &self.permit) {
            Ok(()) => self.phase = AdmissionPhase::Enable(index + 1),
            Err(error) => {
                step.transport_fault = is_transport_fault(&error);
                self.fail(
                    Some(address),
                    Pic16AdmissionStage::EnableVoltage,
                    error.to_string(),
                );
            }
        }
    }

    fn final_heartbeat(
        &mut self,
        i2c: &mut I2cBus,
        mut index: usize,
        step: &mut Pic16AdmissionStep,
    ) {
        while index < self.endpoints.len() && !self.endpoints[index].active() {
            index += 1;
        }
        if index >= self.endpoints.len() {
            self.phase = AdmissionPhase::Deliver;
            return;
        }
        let address = self.endpoints[index].address;
        match execute_heartbeat(i2c, address, I2cPicFirmware::Unknown, &self.permit) {
            Ok(()) => self.phase = AdmissionPhase::FinalHeartbeat(index + 1),
            Err(error) => {
                step.transport_fault = is_transport_fault(&error);
                self.fail(
                    Some(address),
                    Pic16AdmissionStage::FinalHeartbeat,
                    error.to_string(),
                );
            }
        }
    }

    fn deliver_success(&mut self, now: Instant) {
        if !self
            .permit
            .authority
            .validate(self.permit.intent, self.permit.generation)
        {
            self.fail(
                None,
                Pic16AdmissionStage::BatchPublication,
                "admission generation changed before batch publication",
            );
            return;
        }
        let Some(batch) = self.published_batch.as_ref().map(Arc::clone) else {
            self.fail(
                None,
                Pic16AdmissionStage::BatchPublication,
                "admission reached batch publication without retained shutdown authority",
            );
            return;
        };
        let runtime_liveness = self
            .endpoints
            .iter()
            .filter_map(|endpoint| {
                endpoint
                    .running_fence
                    .as_ref()
                    .map(|fence| (endpoint.address, Weak::clone(&fence.live_chain_lease)))
            })
            .collect();
        if let Err(detail) = batch.install_runtime_liveness(runtime_liveness) {
            self.fail(
                None,
                Pic16AdmissionStage::BatchPublication,
                format!("PIC16 batch runtime liveness scope is invalid: {detail}"),
            );
            return;
        }
        let endpoints = self
            .endpoints
            .iter()
            .enumerate()
            .filter_map(|(ordinal, endpoint)| {
                endpoint.evidence.map(|evidence| Pic16AdmittedEndpoint {
                    id: Pic16RuntimeEndpointId {
                        ordinal,
                        batch: Arc::downgrade(&batch),
                    },
                    address: endpoint.address,
                    evidence,
                    mode: endpoint.mode,
                })
            })
            .collect();
        let adoption_state = Arc::new(std::sync::atomic::AtomicU8::new(PIC16_ADOPTION_OFFERED));
        let (final_tx, final_rx) = mpsc::sync_channel(1);
        let delivery = Pic16AdmissionDelivery {
            result: Ok(Pic16AdmittedBatch {
                bus: self.bus,
                generation: self.permit.generation,
                authority: Arc::downgrade(&self.permit.authority),
                batch,
                endpoints,
                drop_safe_off_service: None,
                _not_sync: std::marker::PhantomData,
            }),
            adoption: Some(Pic16AdmissionAdoption {
                state: Arc::clone(&adoption_state),
                final_rx,
            }),
        };
        let delivered = self
            .completion_tx
            .take()
            .is_some_and(|tx| tx.send(delivery).is_ok());
        if delivered {
            self.phase = AdmissionPhase::AwaitAdoption {
                state: adoption_state,
                final_tx,
                deadline: now + PIC16_ADOPTION_WAIT,
            };
        } else {
            self.fail(
                None,
                Pic16AdmissionStage::BatchPublication,
                "admission result receiver disappeared before provisional delivery",
            );
        }
    }

    fn await_adoption(
        &mut self,
        state: Arc<std::sync::atomic::AtomicU8>,
        final_tx: mpsc::SyncSender<std::result::Result<(), Pic16AdmissionFailure>>,
        deadline: Instant,
        now: Instant,
    ) {
        match state.load(Ordering::SeqCst) {
            PIC16_ADOPTION_CALLER_READY => {
                match state.compare_exchange(
                    PIC16_ADOPTION_CALLER_READY,
                    PIC16_ADOPTION_COMMITTING,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => {
                        self.release_active_state();
                        self.phase = AdmissionPhase::Finished;
                        state.store(PIC16_ADOPTION_COMMITTED, Ordering::SeqCst);
                        let _ = final_tx.send(Ok(()));
                    }
                    Err(_) => {
                        self.phase = AdmissionPhase::AwaitAdoption {
                            state,
                            final_tx,
                            deadline,
                        };
                    }
                }
            }
            PIC16_ADOPTION_OFFERED if now < deadline => {
                self.phase = AdmissionPhase::AwaitAdoption {
                    state,
                    final_tx,
                    deadline,
                }
            }
            PIC16_ADOPTION_OFFERED | PIC16_ADOPTION_ABORTED => {
                self.adoption_confirmation_tx = Some(final_tx);
                self.fail(
                    None,
                    Pic16AdmissionStage::BatchAdoption,
                    if now >= deadline {
                        "caller did not adopt the provisional batch before the worker deadline"
                    } else {
                        "caller aborted provisional batch adoption"
                    },
                );
            }
            PIC16_ADOPTION_COMMITTED => self.finish(),
            observed => {
                self.adoption_confirmation_tx = Some(final_tx);
                self.fail(
                    None,
                    Pic16AdmissionStage::BatchAdoption,
                    format!("invalid batch adoption state {observed}"),
                );
            }
        }
    }

    fn compensate(
        &mut self,
        i2c: &mut I2cBus,
        index: usize,
        retry: bool,
        step: &mut Pic16AdmissionStep,
    ) {
        if index == 0 {
            self.complete_compensation();
            return;
        }
        let address = self.safe_off_order[index - 1];
        let permit = self
            .compensation_permit
            .as_ref()
            .expect("compensation permit is installed");
        match execute_disable_voltage(i2c, address, I2cPicFirmware::Unknown, permit) {
            Ok(()) => self.compensation.push(Pic16CompensationOutcome {
                address,
                status: Pic16CompensationStatus::Disabled,
            }),
            Err(error) if !retry && is_transport_fault(&error) => {
                step.transport_fault = is_transport_fault(&error);
                self.phase = AdmissionPhase::Compensate { index, retry: true };
                return;
            }
            Err(error) => {
                step.transport_fault = is_transport_fault(&error);
                self.compensation.push(Pic16CompensationOutcome {
                    address,
                    status: Pic16CompensationStatus::OutcomeUnknown {
                        detail: error.to_string(),
                    },
                });
            }
        }
        self.phase = AdmissionPhase::Compensate {
            index: index - 1,
            retry: false,
        };
    }

    fn deliver_failure(&mut self) {
        let mut failure = self
            .primary_failure
            .take()
            .unwrap_or(Pic16AdmissionFailure {
                bus: self.bus,
                address: None,
                stage: Pic16AdmissionStage::Admission,
                detail: "admission ended without a primary outcome".into(),
                compensation: Vec::new(),
                cleanup_pending: false,
            });
        failure.compensation = std::mem::take(&mut self.compensation);
        let completion_tx = self.completion_tx.take();
        let adoption_confirmation_tx = self.adoption_confirmation_tx.take();
        self.finish();
        if let Some(tx) = completion_tx {
            let _ = tx.send(Pic16AdmissionDelivery {
                result: Err(failure),
                adoption: None,
            });
        } else if let Some(tx) = adoption_confirmation_tx {
            let _ = tx.send(Err(failure));
        }
    }

    fn pre_activation_error(
        &mut self,
        index: usize,
        stage: Pic16AdmissionStage,
        error: HalError,
        step: &mut Pic16AdmissionStep,
    ) {
        step.transport_fault = is_transport_fault(&error);
        self.fail(
            Some(self.endpoints[index].address),
            stage,
            error.to_string(),
        );
    }

    fn any_active(&self) -> bool {
        self.endpoints.iter().any(EndpointState::active)
    }

    fn apply_cancellation_fence(&mut self) {
        if matches!(
            self.phase,
            AdmissionPhase::Compensate { .. }
                | AdmissionPhase::DeliverFailure
                | AdmissionPhase::Finished
        ) {
            return;
        }
        if self.cancellation.load(Ordering::SeqCst) {
            self.retain_adoption_confirmation();
            self.fail(
                None,
                Pic16AdmissionStage::Cancellation,
                "admission owner cancelled or was dropped",
            );
        } else if !self
            .permit
            .authority
            .validate(self.permit.intent, self.permit.generation)
        {
            self.retain_adoption_confirmation();
            self.fail(
                None,
                Pic16AdmissionStage::GenerationFence,
                "admission generation was superseded by SafeOff or terminal shutdown",
            );
        } else if let Some(address) = self.endpoints.iter().find_map(|endpoint| {
            (!endpoint.running_fence_is_current(
                self.bus,
                &self.permit.authority,
                self.permit.generation,
            ))
            .then_some(endpoint.address)
        }) {
            self.retain_adoption_confirmation();
            self.fail(
                Some(address),
                Pic16AdmissionStage::RunningEvidence,
                "running-handoff liveness evidence expired during admission",
            );
        }
    }

    fn complete_compensation(&mut self) {
        if self.reconcile_released_published_batch() {
            return;
        }
        let unresolved = self.compensation.iter().any(|outcome| {
            matches!(
                outcome.status,
                Pic16CompensationStatus::OutcomeUnknown { .. }
            )
        });
        if unresolved {
            self.permit.authority.mark_pic16_shutdown_unresolved();
        } else if let Some(batch) = self.published_batch.as_ref().map(Arc::clone) {
            if !batch.claim_worker_safe_off_attempt() {
                // A caller has claimed or queued the exact batch SafeOff.
                // Defer release so the worker's next mailbox turn can either
                // execute that attempt or observe its enqueue failure. This
                // linearizes caller errors with admission compensation.
                self.phase = AdmissionPhase::Compensate {
                    index: 0,
                    retry: false,
                };
                return;
            }
            self.published_batch.take();
            let epoch = batch.epoch();
            let active_epoch = self
                .permit
                .authority
                .pic16_active_batch_epoch
                .load(Ordering::SeqCst);
            if batch.released() && active_epoch == 0 {
                // A concurrent service-owned batch SafeOff may have already
                // completed after provisional publication but before caller
                // adoption. The exact retained Arc carries that release proof.
            } else if self.permit.authority.release_pic16_batch(epoch).is_err() {
                self.permit.authority.mark_pic16_shutdown_unresolved();
            } else {
                batch.mark_released();
            }
            batch.finish_safe_off_attempt();
        }
        self.phase = AdmissionPhase::DeliverFailure;
    }

    fn retain_adoption_confirmation(&mut self) {
        if let AdmissionPhase::AwaitAdoption { final_tx, .. } = &self.phase {
            self.adoption_confirmation_tx = Some(final_tx.clone());
        }
    }

    fn fail(&mut self, address: Option<u8>, stage: Pic16AdmissionStage, detail: impl Into<String>) {
        if self.primary_failure.is_none() {
            self.primary_failure = Some(Pic16AdmissionFailure {
                bus: self.bus,
                address,
                stage,
                detail: detail.into(),
                compensation: Vec::new(),
                cleanup_pending: false,
            });
        }
        if self.reconcile_released_published_batch() {
            return;
        }
        if self.safe_off_order.is_empty() {
            self.phase = AdmissionPhase::DeliverFailure;
            return;
        }
        if self.compensation_permit.is_none() {
            let generation = self.permit.authority.advance_safe_off_generation();
            let Some(batch) = self.published_batch.as_ref().map(Arc::clone) else {
                self.permit.authority.mark_pic16_shutdown_unresolved();
                if let Some(failure) = self.primary_failure.as_mut() {
                    failure.cleanup_pending = true;
                }
                self.phase = AdmissionPhase::DeliverFailure;
                return;
            };
            self.compensation_permit = Some(I2cSafetyPermit {
                authority: Arc::clone(&self.permit.authority),
                intent: I2cOperationIntent::SafeOff,
                generation,
                scope: super::I2cPermitScope::Pic16BatchSafeOff {
                    epoch: batch.epoch(),
                    batch,
                },
            });
        }
        self.phase = AdmissionPhase::Compensate {
            index: self.safe_off_order.len(),
            retry: false,
        };
    }

    fn reconcile_released_published_batch(&mut self) -> bool {
        let externally_released = self.published_batch.as_ref().is_some_and(|batch| {
            batch.released()
                && self
                    .permit
                    .authority
                    .pic16_active_batch_epoch
                    .load(Ordering::SeqCst)
                    == 0
        });
        if !externally_released {
            return false;
        }

        // The retained exact batch Arc proves that the worker-owned atomic
        // SafeOff completed every endpoint and released this epoch. No later
        // admission operation can re-energize it because that SafeOff also
        // superseded this job's generation. Avoid a redundant DISABLE pass:
        // a later transport failure cannot weaken the already-established
        // proof or turn a clean shutdown into a false quarantine.
        self.published_batch.take();
        self.compensation = self
            .safe_off_order
            .iter()
            .rev()
            .map(|&address| Pic16CompensationOutcome {
                address,
                status: Pic16CompensationStatus::Disabled,
            })
            .collect();
        self.phase = AdmissionPhase::DeliverFailure;
        true
    }

    fn finish(&mut self) {
        self.release_active_state();
        self.phase = AdmissionPhase::Finished;
    }

    fn release_active_state(&mut self) {
        if self.active_state_armed {
            let transitioned = self
                .permit
                .authority
                .pic16_admission_owner
                .compare_exchange(
                    self.reservation_token | PIC16_ADMISSION_ACTIVE_BIT,
                    PIC16_ADMISSION_IDLE,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
            debug_assert!(
                transitioned.is_ok(),
                "active PIC16 admission state changed outside its owner"
            );
            self.active_state_armed = false;
        }
    }
}

impl Drop for Pic16AdmissionState {
    fn drop(&mut self) {
        if self.active_state_armed {
            self.permit.authority.mark_pic16_shutdown_unresolved();
            tracing::error!(
                bus = self.bus,
                shutdown_owned_endpoints = self.safe_off_order.len(),
                "PIC16 admission state dropped before deterministic completion; terminal mutation admission latched"
            );
        }
        self.release_active_state();
    }
}

fn classify_application(raw: u8) -> Option<Pic16ApplicationEvidence> {
    match raw {
        0x56 | 0x5A | 0x5E => Some(Pic16ApplicationEvidence::Stock { version: raw }),
        0x03 => Some(Pic16ApplicationEvidence::BraiinsOs),
        0x60 => Some(Pic16ApplicationEvidence::ApplicationModeUnknown),
        _ => None,
    }
}

fn is_transport_fault(error: &HalError) -> bool {
    matches!(error, HalError::I2c { .. })
}

pub(super) fn hal_busy_error(bus: u8, address: u8) -> HalError {
    HalError::I2cAdmissionBusy {
        bus,
        addr: address,
        detail: "PIC16 worker job owns the service; ordinary I2C work is excluded".into(),
    }
}

pub(super) fn validate_admitted_batch_for_service(
    service_bus: u8,
    service_authority: &Arc<I2cSafetyAuthority>,
    admitted: &Pic16AdmittedBatch,
    intent: I2cOperationIntent,
) -> Result<()> {
    let first_address = admitted
        .endpoints
        .first()
        .map_or(0, |endpoint| endpoint.address);
    if admitted.bus != service_bus {
        return Err(HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: first_address,
            detail: format!(
                "PIC16 admitted batch is bound to bus {}, not service bus {}",
                admitted.bus, service_bus
            ),
        });
    }
    let authority = admitted
        .authority()
        .ok_or_else(|| HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: first_address,
            detail: "PIC16 admitted batch belongs to a dropped I2C service".into(),
        })?;
    if !Arc::ptr_eq(&authority, service_authority) {
        return Err(HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: first_address,
            detail: "PIC16 admitted batch belongs to a different I2C service allocation".into(),
        });
    }
    if intent.requires_current_safety_generation()
        && !authority.validate(intent, admitted.generation)
    {
        return Err(HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: first_address,
            detail: format!(
                "PIC16 admitted batch generation {} is no longer current",
                admitted.generation
            ),
        });
    }
    if intent != I2cOperationIntent::SafeOff && !admitted.batch.runtime_liveness_is_current() {
        let detail = admitted
            .batch
            .expired_runtime_liveness_address()
            .map_or_else(
                || "PIC16 batch runtime liveness scope is unavailable".to_string(),
                |address| {
                    format!(
                        "PIC16 batch lost aggregate live-chain authority at endpoint 0x{address:02X}"
                    )
                },
            );
        return Err(HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: first_address,
            detail,
        });
    }
    if intent != I2cOperationIntent::SafeOff
        && authority.pic16_active_batch_epoch.load(Ordering::SeqCst) != admitted.batch.epoch()
    {
        return Err(HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: first_address,
            detail: format!(
                "PIC16 admitted batch epoch {} is no longer active",
                admitted.batch.epoch()
            ),
        });
    }
    if admitted.endpoints.len() != admitted.batch.addresses().len()
        || admitted
            .endpoints
            .iter()
            .map(|endpoint| endpoint.address)
            .ne(admitted.batch.addresses().iter().copied())
    {
        return Err(HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: first_address,
            detail: "PIC16 admitted batch endpoint set no longer matches retained authority".into(),
        });
    }
    Ok(())
}

pub(super) fn validate_safe_off_handle_for_service(
    service_bus: u8,
    service_authority: &Arc<I2cSafetyAuthority>,
    handle: &Pic16SafeOffHandle,
) -> Result<()> {
    if handle.bus != service_bus {
        return Err(HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: handle.addresses().first().copied().unwrap_or(0),
            detail: format!(
                "PIC16 SafeOff handle is bound to bus {}, not service bus {}",
                handle.bus, service_bus
            ),
        });
    }
    let authority = handle
        .authority()
        .ok_or_else(|| HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: handle.addresses().first().copied().unwrap_or(0),
            detail: "PIC16 SafeOff handle belongs to a dropped I2C service".into(),
        })?;
    if !Arc::ptr_eq(&authority, service_authority) {
        return Err(HalError::I2cSafetySuperseded {
            bus: service_bus,
            addr: handle.addresses().first().copied().unwrap_or(0),
            detail: "PIC16 SafeOff handle belongs to a different I2C service allocation".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::{request_conflicts_with_pic16_authority, I2cPermitScope, I2cRequest};

    #[test]
    fn worker_gate_rejects_raced_ordinary_request_while_reserved() {
        let authority = Arc::new(I2cSafetyAuthority::default());
        let reservation = Pic16AdmissionReservation::reserve(Arc::clone(&authority), 0, 0x55)
            .expect("reserve worker ownership");
        let ordinary_permit = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::ReadOnly,
            generation: 0,
            scope: I2cPermitScope::Generic,
        };
        let (reply_tx, _reply_rx) = mpsc::sync_channel(1);
        let ordinary = I2cRequest::ReadBytes {
            addr: 0x48,
            len: 1,
            reply_tx,
        };
        assert!(request_conflicts_with_pic16_authority(
            &ordinary,
            &ordinary_permit
        ));

        let safe_off_permit = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::SafeOff,
            generation: 0,
            scope: I2cPermitScope::Generic,
        };
        let (reply_tx, _reply_rx) = mpsc::sync_channel(1);
        let safe_off = I2cRequest::DisableVoltage {
            addr: 0x55,
            firmware: I2cPicFirmware::Unknown,
            reply_tx,
        };
        assert!(!request_conflicts_with_pic16_authority(
            &safe_off,
            &safe_off_permit
        ));
        drop(reservation);
        assert_eq!(
            authority.pic16_admission_owner.load(Ordering::SeqCst),
            PIC16_ADMISSION_IDLE
        );
    }

    #[test]
    fn revoked_queued_reservation_cannot_activate_a_new_owner_token() {
        let authority = Arc::new(I2cSafetyAuthority::default());
        let stale = Pic16AdmissionReservation::reserve(Arc::clone(&authority), 0, 0x55)
            .expect("reserve stale owner");
        let stale_token = stale.token();
        Pic16AdmissionReservation::revoke(&authority, stale_token);
        assert_eq!(
            authority.pic16_admission_owner.load(Ordering::SeqCst),
            PIC16_ADMISSION_IDLE
        );

        let current = Pic16AdmissionReservation::reserve(Arc::clone(&authority), 0, 0x55)
            .expect("reserve current owner");
        let current_token = current.token();
        assert!(stale.activate().is_err());
        current.activate().expect("current reservation activates");
        assert_eq!(
            authority.pic16_admission_owner.load(Ordering::SeqCst),
            current_token | PIC16_ADMISSION_ACTIVE_BIT
        );
        authority
            .pic16_admission_owner
            .store(PIC16_ADMISSION_IDLE, Ordering::SeqCst);
    }

    #[test]
    fn managed_history_fences_queued_and_between_stage_raw_work_but_preserves_exact_scopes() {
        let authority = Arc::new(I2cSafetyAuthority::default());
        let generic = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::ReadOnly,
            generation: 0,
            scope: I2cPermitScope::Generic,
        };
        generic
            .validate_admission(0, 0x55)
            .expect("raw work is initially authorized");
        drop(
            generic
                .begin_stage(0, 0x55, "pre-publication stage")
                .expect("first raw stage starts before publication"),
        );

        let reservation = Pic16AdmissionReservation::reserve(Arc::clone(&authority), 0, 0x55)
            .expect("reserve exact admission");
        let reservation_token = reservation.token();
        let pre_management_safe_off = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::SafeOff,
            generation: 0,
            scope: I2cPermitScope::Pic16PreManagementSafeOff { address: 0x55 },
        };
        let batch = authority
            .publish_pic16_batch(0, vec![0x55])
            .expect("publish managed address");
        let admission = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::Energize,
            generation: 0,
            scope: I2cPermitScope::Pic16Admission {
                epoch: batch.epoch(),
                batch: Arc::clone(&batch),
                reservation_token,
            },
        };

        assert!(matches!(
            generic.validate_admission(0, 0x55),
            Err(HalError::I2cSafetySuperseded { .. })
        ));
        assert!(matches!(
            generic.begin_stage(0, 0x55, "post-publication stage"),
            Err(HalError::I2cSafetySuperseded { .. })
        ));
        pre_management_safe_off
            .begin_stage(0, 0x55, "accepted pre-publication SafeOff")
            .expect("already accepted SafeOff remains preemptive");
        admission
            .begin_stage(0, 0x55, "exact admission")
            .expect("exact admission owns its batch address");
        assert!(admission
            .begin_stage(0, 0x56, "unrelated admission address")
            .is_err());

        authority
            .release_pic16_batch(batch.epoch())
            .expect("release exact batch");
        batch.mark_released();
        drop(reservation);
        assert!(matches!(
            generic.validate_admission(0, 0x55),
            Err(HalError::I2cSafetySuperseded { .. })
        ));
        generic
            .validate_admission(0, 0x56)
            .expect("unmanaged sibling remains available");
    }

    #[test]
    fn pre_management_safe_off_handoff_linearizes_before_batch_publication() {
        let (service, _receiver_guard) = I2cServiceHandle::for_unit_tests();
        let authority = Arc::clone(&service.safety);
        let (commit_entered_tx, commit_entered_rx) = mpsc::sync_channel(0);
        let (release_commit_tx, release_commit_rx) = mpsc::sync_channel(0);
        let (publisher_ready_tx, publisher_ready_rx) = mpsc::sync_channel(0);
        let (published_tx, published_rx) = mpsc::sync_channel(0);

        std::thread::scope(|scope| {
            let committed = scope.spawn(move || {
                service.commit_pre_management_safe_off(0x55, |_permit| {
                    commit_entered_tx
                        .send(())
                        .expect("announce committed SafeOff handoff");
                    release_commit_rx
                        .recv()
                        .expect("release committed SafeOff handoff");
                    Ok(())
                })
            });
            commit_entered_rx
                .recv()
                .expect("SafeOff did not enter its atomic handoff");
            assert_eq!(authority.generation.load(Ordering::SeqCst), 1);

            let publishing_authority = Arc::clone(&authority);
            let publisher = scope.spawn(move || {
                publisher_ready_tx
                    .send(())
                    .expect("announce publication attempt");
                let batch = publishing_authority
                    .publish_pic16_batch(0, vec![0x55])
                    .expect("publish after committed SafeOff");
                published_tx
                    .send(Arc::clone(&batch))
                    .expect("return published batch");
                batch
            });
            publisher_ready_rx
                .recv()
                .expect("publisher did not reach the authority boundary");
            assert!(
                matches!(
                    authority.pic16_service_state.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ),
                "SafeOff handoff did not retain the publication lock"
            );

            release_commit_tx
                .send(())
                .expect("finish committed SafeOff handoff");
            committed
                .join()
                .expect("SafeOff handoff thread")
                .expect("SafeOff handoff result");
            let published = published_rx
                .recv()
                .expect("publication did not resume after handoff");
            let returned = publisher.join().expect("publisher thread");
            assert!(Arc::ptr_eq(&published, &returned));
            authority
                .release_pic16_batch(published.epoch())
                .expect("release atomic-handoff test batch");
            published.mark_released();
        });
    }

    #[test]
    fn worker_gate_rejects_generic_pic16_mutation_after_batch_publication() {
        let authority = Arc::new(I2cSafetyAuthority::default());
        let batch = authority
            .publish_pic16_batch(0, vec![0x55])
            .expect("publish test batch");
        let (reply_tx, _reply_rx) = mpsc::sync_channel(1);
        let request = I2cRequest::Heartbeat {
            addr: 0x55,
            firmware: I2cPicFirmware::Unknown,
            reply_tx,
        };
        let generic = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::KeepAlive,
            generation: 0,
            scope: I2cPermitScope::Generic,
        };
        assert!(request_conflicts_with_pic16_authority(&request, &generic));

        let (reply_tx, _reply_rx) = mpsc::sync_channel(1);
        let raw_read = I2cRequest::ReadBytes {
            addr: 0x55,
            len: 1,
            reply_tx,
        };
        let generic_read = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::ReadOnly,
            generation: 0,
            scope: I2cPermitScope::Generic,
        };
        assert!(request_conflicts_with_pic16_authority(
            &raw_read,
            &generic_read
        ));

        let (reply_tx, _reply_rx) = mpsc::sync_channel(1);
        let individual_safe_off = I2cRequest::DisableVoltage {
            addr: 0x55,
            firmware: I2cPicFirmware::Unknown,
            reply_tx,
        };
        let generic_safe_off = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::SafeOff,
            generation: 1,
            scope: I2cPermitScope::Generic,
        };
        assert!(request_conflicts_with_pic16_authority(
            &individual_safe_off,
            &generic_safe_off
        ));

        let batch_permit = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::KeepAlive,
            generation: 0,
            scope: I2cPermitScope::Pic16RuntimeBatch {
                epoch: batch.epoch(),
                batch: Arc::clone(&batch),
            },
        };
        assert!(!request_conflicts_with_pic16_authority(
            &request,
            &batch_permit
        ));
        authority
            .release_pic16_batch(batch.epoch())
            .expect("release test batch");
        assert!(request_conflicts_with_pic16_authority(&request, &generic));
    }

    #[test]
    fn unwind_while_phase_uses_finished_sentinel_preserves_shutdown_quarantine() {
        let authority = Arc::new(I2cSafetyAuthority::default());
        let reservation = Pic16AdmissionReservation::reserve(Arc::clone(&authority), 0, 0x55)
            .expect("reserve panic-test admission");
        let reservation_token = reservation
            .activate()
            .expect("activate panic-test admission");
        let batch = authority
            .publish_pic16_batch(0, vec![0x55])
            .expect("publish panic-test shutdown ownership");
        let permit = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::Energize,
            generation: 0,
            scope: I2cPermitScope::Pic16Admission {
                epoch: batch.epoch(),
                batch: Arc::clone(&batch),
                reservation_token,
            },
        };
        let cancellation = Arc::new(AtomicBool::new(false));
        let (completion_tx, _completion_rx) = mpsc::sync_channel(1);

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut state = Pic16AdmissionState::new(
                0,
                permit,
                cancellation,
                completion_tx,
                vec![Pic16AdmissionPlan::new(
                    0x55,
                    Pic16AdmissionMode::ProgramAndEnable { pic_value: 100 },
                    None,
                )],
                batch,
                reservation_token,
            );
            // Both advance paths use this temporary sentinel while a phase
            // handler is running. Unwind must key off ownership, not phase.
            state.phase = AdmissionPhase::Finished;
            panic!("injected phase-handler unwind");
        }));

        assert!(unwind.is_err());
        assert!(authority.pic16_shutdown_unresolved.load(Ordering::SeqCst));
        assert_ne!(authority.pic16_active_batch_epoch.load(Ordering::SeqCst), 0);
        assert_eq!(
            authority.pic16_admission_owner.load(Ordering::SeqCst),
            PIC16_ADMISSION_IDLE
        );
    }
}
