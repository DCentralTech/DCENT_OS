//! Worker-owned runtime operations for an admitted PIC16 batch.
//!
//! A heartbeat round is one service job. The worker validates the complete
//! retained batch before every endpoint frame and again on a separate final
//! scheduler turn. Reserved SafeOff work therefore remains preemptive between
//! frames without permitting caller-paced gaps or partial-batch submission.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use super::pic16_admission::{
    Pic16AdmissionState, Pic16BatchAuthority, Pic16HeartbeatRoundOutcome,
};
use super::{
    execute_heartbeat, HalError, I2cBus, I2cPermitScope, I2cPicFirmware, I2cSafetyPermit, Result,
    I2C_REQUEST_FINISHED, PIC16_RUNTIME_MAX_ENDPOINTS,
};

pub(super) struct Pic16WorkerStep {
    pub(super) next_due: Option<Instant>,
    pub(super) transport_fault: bool,
    pub(super) finished: bool,
    pub(super) shutdown_active_batch: bool,
}

impl Pic16WorkerStep {
    fn ready() -> Self {
        Self {
            next_due: None,
            transport_fault: false,
            finished: false,
            shutdown_active_batch: false,
        }
    }
}

pub(super) enum Pic16WorkerJob {
    Admission(Pic16AdmissionState),
    HeartbeatRound(Pic16HeartbeatRoundState),
}

impl Pic16WorkerJob {
    pub(super) fn admission(state: Pic16AdmissionState) -> Self {
        Self::Admission(state)
    }

    pub(super) fn heartbeat_round(state: Pic16HeartbeatRoundState) -> Self {
        Self::HeartbeatRound(state)
    }

    pub(super) fn request_cancel(&self) {
        match self {
            Self::Admission(state) => state.request_cancel(),
            Self::HeartbeatRound(state) => state.request_cancel(),
        }
    }

    pub(super) fn requires_bus(&self) -> bool {
        match self {
            Self::Admission(state) => state.requires_bus(),
            Self::HeartbeatRound(state) => state.requires_bus(),
        }
    }

    pub(super) fn transport_unavailable(&mut self, detail: impl Into<String>) {
        let detail = detail.into();
        match self {
            Self::Admission(state) => state.transport_unavailable(detail),
            Self::HeartbeatRound(state) => state.transport_unavailable(detail),
        }
    }

    pub(super) fn advance(&mut self, i2c: &mut I2cBus, now: Instant) -> Pic16WorkerStep {
        match self {
            Self::Admission(state) => {
                let step = state.advance(i2c, now);
                Pic16WorkerStep {
                    next_due: step.next_due,
                    transport_fault: step.transport_fault,
                    finished: step.finished,
                    shutdown_active_batch: false,
                }
            }
            Self::HeartbeatRound(state) if state.requires_bus() => state.advance(i2c),
            Self::HeartbeatRound(state) => state.advance_without_bus(),
        }
    }

    pub(super) fn advance_without_bus(&mut self, now: Instant) -> Pic16WorkerStep {
        match self {
            Self::Admission(state) => {
                let step = state.advance_without_bus(now);
                Pic16WorkerStep {
                    next_due: step.next_due,
                    transport_fault: step.transport_fault,
                    finished: step.finished,
                    shutdown_active_batch: false,
                }
            }
            Self::HeartbeatRound(state) => state.advance_without_bus(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pic16HeartbeatRoundPhase {
    Heartbeat,
    Finalize,
    PublishFailure,
    Finished,
}

pub(super) struct Pic16HeartbeatRoundState {
    bus: u8,
    permit: I2cSafetyPermit,
    batch: Arc<Pic16BatchAuthority>,
    next_index: usize,
    phase: Pic16HeartbeatRoundPhase,
    deadline: Instant,
    cancellation: AtomicBool,
    pending_failure: Option<HalError>,
    pending_transport_fault: bool,
    reply_tx: Option<mpsc::SyncSender<Result<Pic16HeartbeatRoundOutcome>>>,
    request_state: Arc<AtomicU8>,
}

impl Pic16HeartbeatRoundState {
    pub(super) fn new(
        bus: u8,
        permit: I2cSafetyPermit,
        batch: Arc<Pic16BatchAuthority>,
        reply_tx: mpsc::SyncSender<Result<Pic16HeartbeatRoundOutcome>>,
        request_state: Arc<AtomicU8>,
    ) -> Self {
        let deadline =
            Instant::now() + super::pic16_heartbeat_round_execution_budget(batch.addresses().len());
        let mut state = Self {
            bus,
            permit,
            batch,
            next_index: 0,
            phase: Pic16HeartbeatRoundPhase::Heartbeat,
            deadline,
            cancellation: AtomicBool::new(false),
            pending_failure: None,
            pending_transport_fault: false,
            reply_tx: Some(reply_tx),
            request_state,
        };
        if let Err(error) = state.validate("worker start") {
            state.stage_failure(error, false);
        }
        state
    }

    pub(super) fn request_cancel(&self) {
        self.cancellation.store(true, Ordering::SeqCst);
    }

    pub(super) fn requires_bus(&self) -> bool {
        matches!(self.phase, Pic16HeartbeatRoundPhase::Heartbeat)
    }

    pub(super) fn transport_unavailable(&mut self, detail: impl Into<String>) {
        self.stage_failure(
            HalError::I2c {
                bus: self.bus,
                addr: self.first_address(),
                detail: detail.into(),
            },
            true,
        );
    }

    pub(super) fn advance(&mut self, i2c: &mut I2cBus) -> Pic16WorkerStep {
        debug_assert!(self.requires_bus());
        if !i2c.timeout_is_verified(super::I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES) {
            return self.publish_failure(
                HalError::I2cSafetySuperseded {
                    bus: self.bus,
                    addr: self.first_address(),
                    detail: format!(
                        "PIC16 heartbeat round requires a verified {}-jiffy I2C timeout on the current transport",
                        super::I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES
                    ),
                },
                false,
            );
        }
        if let Err(error) = self.validate("heartbeat frame") {
            return self.publish_failure(error, false);
        }
        let Some(&address) = self.batch.addresses().get(self.next_index) else {
            return self.publish_failure(
                HalError::I2cSafetySuperseded {
                    bus: self.bus,
                    addr: self.first_address(),
                    detail: "PIC16 heartbeat round index escaped its retained batch".into(),
                },
                false,
            );
        };
        match execute_heartbeat(i2c, address, I2cPicFirmware::Unknown, &self.permit) {
            Ok(()) => {
                self.next_index += 1;
                if self.next_index == self.batch.addresses().len() {
                    self.phase = Pic16HeartbeatRoundPhase::Finalize;
                }
                Pic16WorkerStep::ready()
            }
            Err(error) => {
                let transport_fault = matches!(error, HalError::I2c { .. });
                self.publish_failure(error, transport_fault)
            }
        }
    }

    pub(super) fn advance_without_bus(&mut self) -> Pic16WorkerStep {
        match self.phase {
            Pic16HeartbeatRoundPhase::Finalize => match self.validate("final publication") {
                Ok(()) => self.publish_success(),
                Err(error) => self.publish_failure(error, false),
            },
            Pic16HeartbeatRoundPhase::PublishFailure => {
                let error = self.pending_failure.take().unwrap_or_else(|| {
                    HalError::I2cSafeOffOutcomeUnknown {
                        bus: self.bus,
                        addr: self.first_address(),
                        detail: "PIC16 heartbeat round lost its terminal failure".into(),
                    }
                });
                let transport_fault = self.pending_transport_fault;
                self.publish_staged(Err(error), transport_fault)
            }
            Pic16HeartbeatRoundPhase::Finished => Pic16WorkerStep {
                finished: true,
                ..Pic16WorkerStep::ready()
            },
            Pic16HeartbeatRoundPhase::Heartbeat => {
                debug_assert!(
                    false,
                    "busless heartbeat advance requested for a wire phase"
                );
                self.publish_failure(
                    HalError::I2c {
                        bus: self.bus,
                        addr: self.first_address(),
                        detail: "PIC16 heartbeat round lost its I2C transport".into(),
                    },
                    true,
                )
            }
        }
    }

    fn validate(&self, stage: &'static str) -> Result<()> {
        let address = self
            .batch
            .addresses()
            .get(self.next_index)
            .copied()
            .unwrap_or_else(|| self.first_address());
        let endpoint_count = self.batch.addresses().len();
        if !(1..=PIC16_RUNTIME_MAX_ENDPOINTS).contains(&endpoint_count) {
            return Err(HalError::I2cSafetySuperseded {
                bus: self.bus,
                addr: address,
                detail: format!(
                    "PIC16 heartbeat round requires 1..={PIC16_RUNTIME_MAX_ENDPOINTS} retained endpoints, observed {endpoint_count}"
                ),
            });
        }
        if self.cancellation.load(Ordering::SeqCst) {
            return Err(HalError::I2cSafetySuperseded {
                bus: self.bus,
                addr: address,
                detail: format!("PIC16 heartbeat round was cancelled before {stage}"),
            });
        }
        if Instant::now() >= self.deadline {
            return Err(HalError::I2cSafetySuperseded {
                bus: self.bus,
                addr: address,
                detail: format!(
                    "PIC16 heartbeat round exceeded its worker deadline before {stage}"
                ),
            });
        }
        let exact_scope = matches!(
            &self.permit.scope,
            I2cPermitScope::Pic16RuntimeBatch { epoch, batch }
                if *epoch == self.batch.epoch() && Arc::ptr_eq(batch, &self.batch)
        );
        if !exact_scope {
            return Err(HalError::I2cSafetySuperseded {
                bus: self.bus,
                addr: address,
                detail: "PIC16 heartbeat round request does not own the exact admitted batch"
                    .into(),
            });
        }
        if !self.batch.runtime_liveness_is_current() {
            let detail = self
                .batch
                .expired_runtime_liveness_address()
                .map_or_else(
                    || "PIC16 batch runtime liveness scope is unavailable".to_string(),
                    |expired| {
                        format!(
                            "PIC16 batch lost aggregate live-chain authority at endpoint 0x{expired:02X}"
                        )
                    },
                );
            return Err(HalError::I2cSafetySuperseded {
                bus: self.bus,
                addr: address,
                detail,
            });
        }
        let exact_active_batch = self
            .permit
            .authority
            .active_pic16_batch()
            .is_some_and(|active| Arc::ptr_eq(&active, &self.batch));
        if !exact_active_batch {
            return Err(HalError::I2cSafetySuperseded {
                bus: self.bus,
                addr: address,
                detail: "PIC16 heartbeat round batch is no longer the service-retained authority"
                    .into(),
            });
        }
        self.permit.validate_admission(self.bus, address)
    }

    fn first_address(&self) -> u8 {
        self.batch.addresses().first().copied().unwrap_or(0)
    }

    fn stage_failure(&mut self, error: HalError, transport_fault: bool) {
        self.permit.authority.advance_safe_off_generation();
        self.pending_failure = Some(error);
        self.pending_transport_fault = transport_fault;
        self.phase = Pic16HeartbeatRoundPhase::PublishFailure;
    }

    fn publish_failure(&mut self, error: HalError, transport_fault: bool) -> Pic16WorkerStep {
        self.permit.authority.advance_safe_off_generation();
        self.publish_staged(Err(error), transport_fault)
    }

    fn publish_success(&mut self) -> Pic16WorkerStep {
        let outcome = Pic16HeartbeatRoundOutcome::new(
            self.batch.epoch(),
            self.permit.generation,
            self.batch.addresses().to_vec().into(),
        );
        self.publish_staged(Ok(outcome), false)
    }

    fn publish_staged(
        &mut self,
        result: Result<Pic16HeartbeatRoundOutcome>,
        transport_fault: bool,
    ) -> Pic16WorkerStep {
        self.phase = Pic16HeartbeatRoundPhase::Finished;
        self.request_state
            .store(I2C_REQUEST_FINISHED, Ordering::Release);
        let reply_lost = self
            .reply_tx
            .take()
            .is_none_or(|reply_tx| reply_tx.send(result).is_err());
        if reply_lost {
            self.permit.authority.advance_safe_off_generation();
        }
        Pic16WorkerStep {
            next_due: None,
            transport_fault,
            finished: true,
            shutdown_active_batch: reply_lost,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::{
        I2cOperationIntent, I2cSafetyAuthority, I2C_QUEUE_START_BUDGET, I2C_REQUEST_STARTED,
    };
    use std::time::Duration;

    #[cfg(feature = "sim-hal")]
    #[derive(Default)]
    struct TimeoutFailureBackend {
        writes: std::sync::atomic::AtomicUsize,
    }

    #[cfg(feature = "sim-hal")]
    impl crate::i2c::I2cSimBackend for TimeoutFailureBackend {
        fn write(&self, _bus: u8, _addr: u8, data: &[u8]) -> Result<usize> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(data.len())
        }

        fn read(&self, _bus: u8, _addr: u8, buf: &mut [u8]) -> Result<usize> {
            buf.fill(0);
            Ok(buf.len())
        }

        fn write_read(
            &self,
            _bus: u8,
            _addr: u8,
            _write_data: &[u8],
            read_buf: &mut [u8],
        ) -> Result<()> {
            read_buf.fill(0);
            Ok(())
        }

        fn set_timeout(&self, bus: u8, _timeout_jiffies: u32) -> Result<()> {
            Err(HalError::I2c {
                bus,
                addr: 0,
                detail: "injected timeout configuration failure".into(),
            })
        }
    }

    #[cfg(feature = "sim-hal")]
    #[derive(Default)]
    struct RecordingBackend {
        writes: std::sync::Mutex<Vec<(u8, Vec<u8>)>>,
    }

    #[cfg(feature = "sim-hal")]
    impl crate::i2c::I2cSimBackend for RecordingBackend {
        fn write(&self, _bus: u8, addr: u8, data: &[u8]) -> Result<usize> {
            self.writes.lock().unwrap().push((addr, data.to_vec()));
            Ok(data.len())
        }

        fn read(&self, _bus: u8, _addr: u8, buf: &mut [u8]) -> Result<usize> {
            buf.fill(0);
            Ok(buf.len())
        }

        fn write_read(
            &self,
            _bus: u8,
            _addr: u8,
            _write_data: &[u8],
            read_buf: &mut [u8],
        ) -> Result<()> {
            read_buf.fill(0);
            Ok(())
        }
    }

    fn finalization_state(
        endpoint_count: usize,
    ) -> (
        Pic16HeartbeatRoundState,
        mpsc::Receiver<Result<Pic16HeartbeatRoundOutcome>>,
        Arc<I2cSafetyAuthority>,
        Arc<AtomicU8>,
    ) {
        let authority = Arc::new(I2cSafetyAuthority::default());
        let addresses = (0..endpoint_count)
            .map(|ordinal| 0x55 + u8::try_from(ordinal).unwrap())
            .collect::<Vec<_>>();
        let batch = authority.publish_pic16_batch(0, addresses).unwrap();
        batch.install_runtime_liveness(Vec::new()).unwrap();
        let permit = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::KeepAlive,
            generation: 0,
            scope: I2cPermitScope::Pic16RuntimeBatch {
                epoch: batch.epoch(),
                batch: Arc::clone(&batch),
            },
        };
        let request_state = Arc::new(AtomicU8::new(I2C_REQUEST_STARTED));
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let mut state =
            Pic16HeartbeatRoundState::new(0, permit, batch, reply_tx, Arc::clone(&request_state));
        state.next_index = endpoint_count;
        state.phase = Pic16HeartbeatRoundPhase::Finalize;
        (state, reply_rx, authority, request_state)
    }

    #[test]
    fn heartbeat_budget_pins_one_two_and_three_endpoint_worst_cases() {
        assert_eq!(super::super::pic16_heartbeat_round_byte_operations(1), 19);
        assert_eq!(super::super::pic16_heartbeat_round_byte_operations(2), 22);
        assert_eq!(super::super::pic16_heartbeat_round_byte_operations(3), 25);
        assert_eq!(
            super::super::pic16_heartbeat_round_execution_budget(3),
            Duration::from_millis(3_525)
        );
        assert_eq!(
            I2C_QUEUE_START_BUDGET + super::super::pic16_heartbeat_round_execution_budget(3),
            Duration::from_millis(4_525)
        );
    }

    #[test]
    fn final_publication_owns_request_lifecycle_until_complete() {
        let (mut state, reply_rx, _authority, request_state) = finalization_state(3);
        assert_eq!(request_state.load(Ordering::Acquire), I2C_REQUEST_STARTED);

        let step = state.advance_without_bus();

        assert!(step.finished);
        assert!(!step.shutdown_active_batch);
        assert_eq!(request_state.load(Ordering::Acquire), I2C_REQUEST_FINISHED);
        let outcome = reply_rx.recv().unwrap().unwrap();
        assert_eq!(outcome.addresses(), [0x55, 0x56, 0x57]);
    }

    #[test]
    fn lost_success_receiver_fences_generation_and_requests_worker_shutdown() {
        let (mut state, reply_rx, authority, request_state) = finalization_state(2);
        drop(reply_rx);
        let generation = authority.generation.load(Ordering::SeqCst);

        let step = state.advance_without_bus();

        assert!(step.finished);
        assert!(step.shutdown_active_batch);
        assert!(authority.generation.load(Ordering::SeqCst) > generation);
        assert_eq!(request_state.load(Ordering::Acquire), I2C_REQUEST_FINISHED);
    }

    #[test]
    fn finalization_deadline_failure_is_terminal_and_never_publishes_success() {
        let (mut state, reply_rx, authority, request_state) = finalization_state(1);
        state.deadline = Instant::now();

        let step = state.advance_without_bus();

        assert!(step.finished);
        assert!(!step.shutdown_active_batch);
        let error = reply_rx.recv().unwrap().unwrap_err();
        assert!(error.to_string().contains("exceeded its worker deadline"));
        assert!(authority.generation.load(Ordering::SeqCst) > 0);
        assert_eq!(request_state.load(Ordering::Acquire), I2C_REQUEST_FINISHED);
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn unverified_transport_timeout_refuses_before_heartbeat_wire_access() {
        let (mut state, reply_rx, authority, request_state) = finalization_state(1);
        state.next_index = 0;
        state.phase = Pic16HeartbeatRoundPhase::Heartbeat;
        let backend = Arc::new(TimeoutFailureBackend::default());
        let mut i2c = I2cBus::open_sim(0, backend.clone());
        assert!(i2c
            .set_timeout(super::super::I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES)
            .is_err());

        let step = state.advance(&mut i2c);

        assert!(step.finished);
        assert_eq!(backend.writes.load(Ordering::SeqCst), 0);
        let error = reply_rx.recv().unwrap().unwrap_err();
        assert!(error.to_string().contains("requires a verified 10-jiffy"));
        assert!(authority.generation.load(Ordering::SeqCst) > 0);
        assert_eq!(request_state.load(Ordering::Acquire), I2C_REQUEST_FINISHED);
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn mid_round_deadline_stops_before_the_next_endpoint_frame() {
        let (mut state, reply_rx, authority, request_state) = finalization_state(2);
        state.next_index = 0;
        state.phase = Pic16HeartbeatRoundPhase::Heartbeat;
        let backend = Arc::new(RecordingBackend::default());
        let mut i2c = I2cBus::open_sim(0, backend.clone());
        i2c.set_timeout(super::super::I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES)
            .unwrap();

        let first = state.advance(&mut i2c);
        assert!(!first.finished);
        assert_eq!(state.next_index, 1);
        assert_eq!(
            backend.writes.lock().unwrap().as_slice(),
            [(0x55, vec![0x55]), (0x55, vec![0xAA]), (0x55, vec![0x16]),]
        );

        state.deadline = Instant::now();
        let second = state.advance(&mut i2c);

        assert!(second.finished);
        let error = reply_rx.recv().unwrap().unwrap_err();
        assert!(error.to_string().contains("exceeded its worker deadline"));
        assert_eq!(
            backend.writes.lock().unwrap().len(),
            3,
            "deadline expiry must not begin a sibling heartbeat frame"
        );
        assert!(authority.generation.load(Ordering::SeqCst) > 0);
        assert_eq!(request_state.load(Ordering::Acquire), I2C_REQUEST_FINISHED);
    }
}
