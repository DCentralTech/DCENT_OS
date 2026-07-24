#![cfg(feature = "sim-hal")]

use std::time::{Duration, Instant};

use dcentrald_hal::i2c::{
    I2cServiceHandle, Pic16AdmissionMode, Pic16AdmissionStage, Pic16AdmissionTarget,
    Pic16AdmittedBatch, Pic16ApplicationEvidence, Pic16BatchSafeOffDisposition,
    Pic16CompensationStatus,
};
use dcentrald_hal::platform::sim::{
    SimModel, SimPic16Fault, SimPic16Operation, SimPlatform, TraceEvent,
};

fn drain_until(
    platform: &SimPlatform,
    events: &mut Vec<TraceEvent>,
    description: &str,
    predicate: impl Fn(&[TraceEvent]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !predicate(events) {
        events.extend(platform.drain_i2c_trace().expect("drain simulator trace"));
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}; events={events:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn accepted_count(events: &[TraceEvent], operation: SimPic16Operation) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                TraceEvent::Pic16OperationAccepted {
                    operation: observed,
                    ..
                } if *observed == operation
            )
        })
        .count()
}

fn assert_managed_pic16_superseded<T>(result: dcentrald_hal::Result<T>, expected_addr: u8) {
    match result {
        Err(dcentrald_hal::HalError::I2cSafetySuperseded { bus, addr, detail }) => {
            assert_eq!(bus, 0);
            assert_eq!(addr, expected_addr);
            assert!(
                detail.contains("batch release does not restore legacy authority"),
                "managed-address refusal lost its stable policy detail: {detail}"
            );
        }
        Err(error) => panic!("managed address returned the wrong error: {error}"),
        Ok(_) => panic!("managed address unexpectedly accepted raw authority"),
    }
}

fn admit_single_pic16(
    platform: &SimPlatform,
    service: &I2cServiceHandle,
    address: u8,
) -> Pic16AdmittedBatch {
    let _ = platform.drain_i2c_trace().expect("clear simulator trace");
    let endpoint = platform
        .pic16_endpoint(0, address)
        .expect("single PIC16 endpoint");
    let job = service
        .begin_pic16_admission([endpoint], 100)
        .expect("start single admission");
    let mut events = Vec::new();
    drain_until(
        platform,
        &mut events,
        "single heartbeat round 1",
        |events| accepted_count(events, SimPic16Operation::Heartbeat) == 1,
    );
    for round in 2..=5 {
        platform
            .advance_i2c_time(Duration::from_secs(1))
            .expect("advance single heartbeat round");
        drain_until(
            platform,
            &mut events,
            &format!("single heartbeat round {round}"),
            |events| accepted_count(events, SimPic16Operation::Heartbeat) == round,
        );
    }
    drain_until(platform, &mut events, "single SET", |events| {
        accepted_count(events, SimPic16Operation::SetVoltage) == 1
    });
    platform
        .advance_i2c_time(Duration::from_millis(5))
        .expect("advance single enable gap");
    drain_until(platform, &mut events, "single final heartbeat", |events| {
        accepted_count(events, SimPic16Operation::EnableVoltage) == 1
            && accepted_count(events, SimPic16Operation::Heartbeat) == 6
    });
    job.wait().expect("adopt single admission")
}

fn drive_qualification(platform: &SimPlatform, events: &mut Vec<TraceEvent>, endpoints: usize) {
    drain_until(platform, events, "first heartbeat round", |events| {
        accepted_count(events, SimPic16Operation::Heartbeat) == endpoints
    });
    for round in 1..5 {
        platform
            .advance_i2c_time(Duration::from_secs(1))
            .expect("advance qualification clock");
        drain_until(
            platform,
            events,
            &format!("heartbeat round {}", round + 1),
            |events| {
                accepted_count(events, SimPic16Operation::Heartbeat) == endpoints * (round + 1)
            },
        );
    }
}

#[test]
fn batch_runtime_setpoint_is_set_only_and_cannot_recover_a_watchdog_reset() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let mut admitted = admit_single_pic16(&platform, &service, 0x55);
    let endpoint_id = admitted.endpoints()[0].id();
    let _ = platform.drain_i2c_trace().expect("clear admission trace");

    let set = service
        .pic16_set_voltage_in_batch(&mut admitted, &endpoint_id, 0)
        .expect("batch-authorized SET-only request");
    assert_eq!(set.requested_pic_value(), 0);
    assert_eq!(set.canonical_pic_value(), 6);
    let live_snapshot = platform
        .pic16_snapshot(0, 0x55)
        .expect("live runtime setpoint snapshot");
    assert_eq!(live_snapshot.voltage_pic(), Some(6));
    assert!(live_snapshot.voltage_enabled());
    let live_events = platform
        .drain_i2c_trace()
        .expect("drain live runtime setpoint trace");
    assert_eq!(
        accepted_count(&live_events, SimPic16Operation::SetVoltage),
        1
    );
    assert_eq!(
        accepted_count(&live_events, SimPic16Operation::EnableVoltage),
        0
    );

    platform
        .configure_controller_watchdog(Duration::from_millis(50))
        .expect("configure endpoint watchdog");
    platform
        .advance_i2c_time(Duration::from_millis(50))
        .expect("expire endpoint watchdog");
    assert!(!platform
        .pic16_snapshot(0, 0x55)
        .expect("expired endpoint snapshot")
        .voltage_enabled());
    let _ = platform
        .drain_i2c_trace()
        .expect("clear watchdog-expiry trace");

    let error = service
        .pic16_set_voltage_in_batch(&mut admitted, &endpoint_id, 98)
        .expect_err("watchdog-reset endpoint must require fresh admission");
    assert!(error.to_string().contains("raw state 0xCC"));
    let snapshot = platform
        .pic16_snapshot(0, 0x55)
        .expect("rejected runtime setpoint snapshot");
    assert_eq!(snapshot.voltage_pic(), None);
    assert!(
        !snapshot.voltage_enabled(),
        "runtime tuning must not resurrect a watchdog-cut rail"
    );
    let events = platform
        .drain_i2c_trace()
        .expect("drain runtime setpoint trace");
    assert_eq!(accepted_count(&events, SimPic16Operation::SetVoltage), 0);
    assert_eq!(accepted_count(&events, SimPic16Operation::EnableVoltage), 0);

    service
        .pic16_safe_off_admitted_batch(&mut admitted)
        .expect("release runtime-setpoint batch");
}

#[test]
fn runtime_endpoint_id_cannot_cross_admitted_batch_identity() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let mut first = admit_single_pic16(&platform, &service, 0x55);
    let stale_id = first.endpoints()[0].id();
    assert!(service
        .pic16_safe_off_admitted_batch(&mut first)
        .expect("release first batch")
        .all_disabled());

    let mut second = admit_single_pic16(&platform, &service, 0x55);
    let _ = platform
        .drain_i2c_trace()
        .expect("clear second admission trace");
    let error = service
        .pic16_set_voltage_in_batch(&mut second, &stale_id, 99)
        .expect_err("endpoint ID from an older batch must be rejected");
    assert!(error
        .to_string()
        .contains("endpoint ID does not belong to this admitted batch"));
    assert!(second.is_current());
    assert!(platform
        .pic16_snapshot(0, 0x55)
        .expect("second batch remains live")
        .voltage_enabled());
    let events = platform
        .drain_i2c_trace()
        .expect("drain cross-batch rejection trace");
    assert_eq!(accepted_count(&events, SimPic16Operation::SetVoltage), 0);
    assert_eq!(accepted_count(&events, SimPic16Operation::EnableVoltage), 0);
    assert!(service
        .pic16_safe_off_admitted_batch(&mut second)
        .expect("release second batch")
        .all_disabled());
}

#[test]
fn mixed_batch_preserves_proven_running_endpoint_and_programs_only_cold_targets() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let hot_setup = platform.pic16_endpoint(0, 0x56).expect("hot endpoint");
    service
        .pic16_set_and_enable(&hot_setup, 88)
        .expect("establish simulated running handoff");
    platform
        .establish_pic16_live_chain(0, 0x56)
        .expect("establish explicit simulated ASIC liveness");
    let running = platform
        .prove_running_pic16_endpoint(&service, 0, 0x56)
        .expect("mint simulated running evidence");
    let cold_55 = platform.pic16_endpoint(0, 0x55).expect("cold endpoint 55");
    let cold_57 = platform.pic16_endpoint(0, 0x57).expect("cold endpoint 57");
    let _ = platform.drain_i2c_trace().expect("clear setup trace");

    let job = service
        .begin_pic16_admission_batch([
            Pic16AdmissionTarget::program_and_enable(cold_57, 0),
            Pic16AdmissionTarget::continue_proven_running(running),
            Pic16AdmissionTarget::program_and_enable(cold_55, 100),
        ])
        .expect("start mixed admission");
    let mut events = Vec::new();
    drive_qualification(&platform, &mut events, 3);
    drain_until(&platform, &mut events, "cold-only SET frames", |events| {
        accepted_count(events, SimPic16Operation::SetVoltage) == 2
    });
    platform
        .advance_i2c_time(Duration::from_millis(5))
        .expect("advance mixed enable gap");
    drain_until(&platform, &mut events, "mixed final heartbeat", |events| {
        accepted_count(events, SimPic16Operation::EnableVoltage) == 2
            && accepted_count(events, SimPic16Operation::Heartbeat) == 18
    });
    let mut admitted = job.wait().expect("adopt mixed batch");

    assert_eq!(
        admitted
            .endpoints()
            .iter()
            .map(|endpoint| (endpoint.address(), endpoint.mode()))
            .collect::<Vec<_>>(),
        [
            (
                0x55,
                Pic16AdmissionMode::ProgramAndEnable { pic_value: 100 }
            ),
            (0x56, Pic16AdmissionMode::ContinueProvenRunning),
            (0x57, Pic16AdmissionMode::ProgramAndEnable { pic_value: 6 }),
        ]
    );
    assert_eq!(
        platform
            .pic16_snapshot(0, 0x56)
            .expect("preserved endpoint snapshot")
            .voltage_pic(),
        Some(88)
    );
    let set_addresses = events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Pic16OperationAccepted {
                addr,
                operation: SimPic16Operation::SetVoltage,
                ..
            } => Some(*addr),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(set_addresses, [0x55, 0x57]);
    assert_eq!(
        platform
            .pic16_snapshot(0, 0x57)
            .expect("clamped endpoint snapshot")
            .voltage_pic(),
        Some(6)
    );

    let _ = platform
        .drain_i2c_trace()
        .expect("clear admission trace before rejected runtime SET");
    let continue_id = admitted.endpoints()[1].id();
    let rejected = service
        .pic16_set_voltage_in_batch(&mut admitted, &continue_id, 99)
        .expect_err("continue-running endpoint must not authorize SET");
    assert!(rejected
        .to_string()
        .contains("continue-running PIC16 endpoint does not authorize runtime SET"));
    let rejected_events = platform
        .drain_i2c_trace()
        .expect("drain rejected runtime SET trace");
    assert_eq!(
        accepted_count(&rejected_events, SimPic16Operation::SetVoltage),
        0
    );

    platform
        .invalidate_pic16_live_chain(0, 0x56)
        .expect("revoke admitted continue-mode chain lease");
    let heartbeat_error = service
        .pic16_heartbeat_round(&mut admitted)
        .expect_err("expired continue-mode lease must reject heartbeat");
    assert!(heartbeat_error
        .to_string()
        .contains("lost aggregate live-chain authority at endpoint 0x56"));
    let expired_lease_events = platform
        .drain_i2c_trace()
        .expect("drain expired batch heartbeat trace");
    assert_eq!(
        accepted_count(&expired_lease_events, SimPic16Operation::Heartbeat),
        0
    );
    assert_eq!(
        accepted_count(&expired_lease_events, SimPic16Operation::DisableVoltage),
        3,
        "aggregate liveness loss must SafeOff the full batch"
    );

    assert_eq!(
        service
            .pic16_safe_off_admitted_batch(&mut admitted)
            .expect("inspect released mixed batch")
            .disposition(),
        Pic16BatchSafeOffDisposition::AlreadyReleased
    );
}

#[test]
fn stale_running_evidence_is_compensated_under_worker_ownership() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let setup = platform.pic16_endpoint(0, 0x55).expect("setup endpoint");
    service
        .pic16_set_and_enable(&setup, 88)
        .expect("establish simulated running handoff");
    platform
        .establish_pic16_live_chain(0, 0x55)
        .expect("establish explicit simulated ASIC liveness");
    let running = platform
        .prove_running_pic16_endpoint(&service, 0, 0x55)
        .expect("mint simulated running evidence");
    service
        .pic16_set_and_enable(&setup, 89)
        .expect("invalidate liveness while leaving the application rail on");
    assert!(platform
        .pic16_snapshot(0, 0x55)
        .expect("stale powered endpoint snapshot")
        .voltage_enabled());
    let _ = platform.drain_i2c_trace().expect("clear setup trace");

    let job = service
        .begin_pic16_admission_batch([Pic16AdmissionTarget::continue_proven_running(running)])
        .expect("worker must own cleanup before rejecting stale proof");
    let failure = job.wait().expect_err("stale proof must fail closed");
    assert_eq!(failure.stage(), Pic16AdmissionStage::RunningEvidence);
    assert_eq!(failure.address(), Some(0x55));
    assert!(failure
        .detail()
        .contains("liveness evidence expired during admission"));
    assert_eq!(failure.compensation().len(), 1);
    let events = platform.drain_i2c_trace().expect("drain failed admission");
    assert_eq!(accepted_count(&events, SimPic16Operation::RawRead), 0);
    assert_eq!(
        accepted_count(&events, SimPic16Operation::JumpFromLoader),
        0
    );
    assert_eq!(accepted_count(&events, SimPic16Operation::SetVoltage), 0);
    assert_eq!(accepted_count(&events, SimPic16Operation::EnableVoltage), 0);
    assert_eq!(
        accepted_count(&events, SimPic16Operation::DisableVoltage),
        1
    );
    assert!(!platform
        .pic16_snapshot(0, 0x55)
        .expect("compensated endpoint snapshot")
        .voltage_enabled());
}

#[test]
fn running_evidence_loss_during_admission_fails_the_whole_batch() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let hot_setup = platform.pic16_endpoint(0, 0x56).expect("hot endpoint");
    service
        .pic16_set_and_enable(&hot_setup, 88)
        .expect("establish simulated running handoff");
    platform
        .establish_pic16_live_chain(0, 0x56)
        .expect("establish explicit simulated ASIC liveness");
    let running = platform
        .prove_running_pic16_endpoint(&service, 0, 0x56)
        .expect("mint simulated running evidence");
    let cold = platform.pic16_endpoint(0, 0x55).expect("cold endpoint");
    let _ = platform.drain_i2c_trace().expect("clear setup trace");

    let job = service
        .begin_pic16_admission_batch([
            Pic16AdmissionTarget::program_and_enable(cold, 100),
            Pic16AdmissionTarget::continue_proven_running(running),
        ])
        .expect("start mixed admission");
    let mut events = Vec::new();
    drain_until(
        &platform,
        &mut events,
        "initial mixed observation",
        |events| accepted_count(events, SimPic16Operation::RawRead) >= 1,
    );
    platform
        .invalidate_pic16_live_chain(0, 0x56)
        .expect("invalidate ASIC liveness without changing controller state");
    let failed_chain = platform
        .pic16_snapshot(0, 0x56)
        .expect("chain-only failure snapshot");
    assert_eq!(failed_chain.raw_state(), 0x60);
    assert!(failed_chain.voltage_enabled());
    assert_eq!(failed_chain.voltage_pic(), Some(88));
    assert!(!failed_chain.chain_live());
    let failure = job.wait().expect_err("expired live-chain lease must fail");
    assert_eq!(failure.stage(), Pic16AdmissionStage::RunningEvidence);
    assert_eq!(failure.address(), Some(0x56));
    assert!(failure
        .detail()
        .contains("liveness evidence expired during admission"));
    assert_eq!(failure.compensation().len(), 2);
    events.extend(platform.drain_i2c_trace().expect("drain compensation"));
    assert_eq!(accepted_count(&events, SimPic16Operation::SetVoltage), 0);
    assert_eq!(accepted_count(&events, SimPic16Operation::EnableVoltage), 0);
}

#[test]
fn pre_worker_generation_rejection_safes_off_the_provisional_live_batch() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let hot_setup = platform.pic16_endpoint(0, 0x55).expect("hot endpoint");
    service
        .pic16_set_and_enable(&hot_setup, 88)
        .expect("establish simulated running handoff");
    platform
        .establish_pic16_live_chain(0, 0x55)
        .expect("establish explicit simulated ASIC liveness");
    let running = platform
        .prove_running_pic16_endpoint(&service, 0, 0x55)
        .expect("mint simulated running evidence");
    let _ = platform.drain_i2c_trace().expect("clear setup trace");

    platform
        .arm_next_i2c_transfer_stall()
        .expect("arm worker stall");
    let stalled_service = service.clone();
    let stalled = std::thread::spawn(move || {
        stalled_service.heartbeat(0x57, dcentrald_hal::i2c::I2cPicFirmware::Unknown)
    });
    assert!(platform
        .wait_for_i2c_transfer_stall(Duration::from_secs(1))
        .expect("wait for worker stall"));

    let queued_service = service.clone();
    let queued_raw = std::thread::spawn(move || queued_service.read_bytes(0x55, 1));
    std::thread::sleep(Duration::from_millis(10));
    assert!(
        !queued_raw.is_finished(),
        "raw request did not remain queued behind the stalled worker"
    );

    let admission_service = service.clone();
    let admission = std::thread::spawn(move || {
        admission_service
            .begin_pic16_admission_batch([Pic16AdmissionTarget::continue_proven_running(running)])
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if service.pic16_active_batch_addresses().as_deref() == Some(&[0x55]) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "admission did not publish provisional shutdown ownership"
        );
        std::thread::yield_now();
    }

    service.latch_terminal_safe_off();
    platform
        .release_i2c_transfer_stall()
        .expect("release worker stall");
    let _ = stalled.join().expect("stalled request thread");
    assert_managed_pic16_superseded(queued_raw.join().expect("queued raw request thread"), 0x55);
    let _start_error = admission
        .join()
        .expect("admission start thread")
        .expect_err("stale queued admission must not start");
    assert_eq!(service.pic16_active_batch_addresses(), None);
    let events = platform.drain_i2c_trace().expect("drain cleanup trace");
    assert_eq!(
        accepted_count(&events, SimPic16Operation::DisableVoltage),
        1
    );
    assert_eq!(accepted_count(&events, SimPic16Operation::RawRead), 0);
    assert!(!platform
        .pic16_snapshot(0, 0x55)
        .expect("provisionally compensated endpoint")
        .voltage_enabled());
}

#[test]
fn committed_pre_management_safe_off_precedes_published_admission() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let endpoint = platform.pic16_endpoint(0, 0x55).expect("endpoint 55");
    platform
        .arm_next_i2c_transfer_stall()
        .expect("arm worker stall");
    let stalled_service = service.clone();
    let stalled = std::thread::spawn(move || {
        stalled_service.heartbeat(0x57, dcentrald_hal::i2c::I2cPicFirmware::Unknown)
    });
    assert!(platform
        .wait_for_i2c_transfer_stall(Duration::from_secs(1))
        .expect("wait for worker stall"));

    let safe_off_service = service.clone();
    let committed_safe_off = std::thread::spawn(move || {
        safe_off_service.disable_voltage(0x55, dcentrald_hal::i2c::I2cPicFirmware::Unknown)
    });
    std::thread::sleep(Duration::from_millis(10));
    assert!(
        !committed_safe_off.is_finished(),
        "pre-management SafeOff did not remain pending behind the stalled worker"
    );

    let admission_service = service.clone();
    let admission =
        std::thread::spawn(move || admission_service.begin_pic16_admission([endpoint], 100));
    let deadline = Instant::now() + Duration::from_secs(1);
    while service.pic16_active_batch_addresses().as_deref() != Some(&[0x55]) {
        assert!(
            Instant::now() < deadline,
            "admission did not publish behind committed SafeOff"
        );
        std::thread::yield_now();
    }
    assert_managed_pic16_superseded(
        service.disable_voltage(0x55, dcentrald_hal::i2c::I2cPicFirmware::Unknown),
        0x55,
    );

    platform
        .release_i2c_transfer_stall()
        .expect("release worker stall");
    stalled
        .join()
        .expect("stalled worker request")
        .expect("stalled heartbeat result");
    committed_safe_off
        .join()
        .expect("committed SafeOff thread")
        .expect("committed SafeOff executes before admission work");
    let job = admission
        .join()
        .expect("admission start thread")
        .expect("admission starts after committed SafeOff");
    assert!(service
        .pic16_safe_off_active_batch()
        .expect("cancel test admission")
        .expect("published test batch")
        .all_disabled());
    assert!(job.wait().is_err());

    let events = platform
        .drain_i2c_trace()
        .expect("drain handoff race trace");
    assert!(
        accepted_count(&events, SimPic16Operation::DisableVoltage) >= 2,
        "committed raw SafeOff and exact batch cleanup must both execute"
    );
}

#[test]
fn queued_continue_heartbeat_revalidates_live_chain_lease_in_worker() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let hot_setup = platform.pic16_endpoint(0, 0x56).expect("hot endpoint");
    service
        .pic16_set_and_enable(&hot_setup, 88)
        .expect("establish simulated running handoff");
    platform
        .establish_pic16_live_chain(0, 0x56)
        .expect("establish explicit simulated ASIC liveness");
    let running = platform
        .prove_running_pic16_endpoint(&service, 0, 0x56)
        .expect("mint simulated running evidence");
    let cold = platform.pic16_endpoint(0, 0x55).expect("cold endpoint");
    let _ = platform.drain_i2c_trace().expect("clear setup trace");
    let job = service
        .begin_pic16_admission_batch([
            Pic16AdmissionTarget::program_and_enable(cold, 100),
            Pic16AdmissionTarget::continue_proven_running(running),
        ])
        .expect("start mixed admission");
    let mut admission_events = Vec::new();
    drive_qualification(&platform, &mut admission_events, 2);
    drain_until(
        &platform,
        &mut admission_events,
        "mixed SET before queued heartbeat test",
        |events| accepted_count(events, SimPic16Operation::SetVoltage) == 1,
    );
    platform
        .advance_i2c_time(Duration::from_millis(5))
        .expect("advance mixed enable gap");
    drain_until(
        &platform,
        &mut admission_events,
        "mixed final heartbeat before queued heartbeat test",
        |events| {
            accepted_count(events, SimPic16Operation::EnableVoltage) == 1
                && accepted_count(events, SimPic16Operation::Heartbeat) == 12
        },
    );
    let mut admitted = job.wait().expect("adopt mixed batch");
    let _ = platform
        .drain_i2c_trace()
        .expect("clear admission trace before queue race");

    platform
        .arm_next_i2c_transfer_stall()
        .expect("arm batch heartbeat stall");
    std::thread::scope(|scope| {
        let stalled = scope.spawn(|| service.pic16_heartbeat_round(&mut admitted));
        assert!(platform
            .wait_for_i2c_transfer_stall(Duration::from_secs(1))
            .expect("wait for batch heartbeat stall"));
        let ordinary = scope.spawn(|| service.read_bytes(0x48, 1));
        std::thread::sleep(Duration::from_millis(10));
        platform
            .invalidate_pic16_live_chain(0, 0x56)
            .expect("revoke sibling liveness during heartbeat round");
        platform
            .release_i2c_transfer_stall()
            .expect("release batch heartbeat stall");
        let error = stalled
            .join()
            .expect("stalled batch heartbeat thread")
            .expect_err("round must fail when sibling liveness expires");
        assert!(error
            .to_string()
            .contains("lost aggregate live-chain authority at endpoint 0x56"));
        let ordinary_result = ordinary.join().expect("ordinary request thread");
        assert!(
            matches!(
                &ordinary_result,
                Err(dcentrald_hal::HalError::I2cAdmissionBusy { .. })
            ),
            "ordinary request was not rejected by the active heartbeat job: {ordinary_result:?}"
        );
    });

    let events = platform.drain_i2c_trace().expect("drain queue-race trace");
    assert_eq!(accepted_count(&events, SimPic16Operation::Heartbeat), 1);
    assert!(
        !admitted.is_current(),
        "one expired Continue lease must revoke the complete batch"
    );
    assert_eq!(
        accepted_count(&events, SimPic16Operation::DisableVoltage),
        2,
        "incomplete round must SafeOff every endpoint"
    );
    assert_eq!(
        service
            .pic16_safe_off_admitted_batch(&mut admitted)
            .expect("inspect released heartbeat-round batch")
            .disposition(),
        Pic16BatchSafeOffDisposition::AlreadyReleased
    );
}

#[test]
fn reserved_safe_off_preempts_heartbeat_round_before_sibling_frame() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let endpoints =
        [0x55, 0x56].map(|address| platform.pic16_endpoint(0, address).expect("batch endpoint"));
    let job = service
        .begin_pic16_admission(endpoints, 100)
        .expect("start two-endpoint admission");
    let mut admission_events = Vec::new();
    drive_qualification(&platform, &mut admission_events, 2);
    drain_until(
        &platform,
        &mut admission_events,
        "two-endpoint SET frames",
        |events| accepted_count(events, SimPic16Operation::SetVoltage) == 2,
    );
    platform
        .advance_i2c_time(Duration::from_millis(5))
        .expect("advance two-endpoint enable gap");
    drain_until(
        &platform,
        &mut admission_events,
        "two-endpoint final heartbeat",
        |events| {
            accepted_count(events, SimPic16Operation::EnableVoltage) == 2
                && accepted_count(events, SimPic16Operation::Heartbeat) == 12
        },
    );
    let mut admitted = job.wait().expect("adopt two-endpoint batch");
    let shutdown = admitted.safe_off_handle();
    let _ = platform
        .drain_i2c_trace()
        .expect("clear admission trace before preemption");

    platform
        .arm_next_i2c_transfer_stall()
        .expect("arm first heartbeat frame stall");
    std::thread::scope(|scope| {
        let round = scope.spawn(|| service.pic16_heartbeat_round(&mut admitted));
        assert!(platform
            .wait_for_i2c_transfer_stall(Duration::from_secs(1))
            .expect("wait for first heartbeat frame stall"));
        let safe_off = scope.spawn(|| service.pic16_safe_off(&shutdown));
        std::thread::sleep(Duration::from_millis(10));
        platform
            .release_i2c_transfer_stall()
            .expect("release first heartbeat frame");
        assert!(safe_off
            .join()
            .expect("reserved SafeOff thread")
            .expect("reserved whole-batch SafeOff")
            .all_disabled());
        let _ = round
            .join()
            .expect("heartbeat round thread")
            .expect_err("SafeOff must cancel the heartbeat round");
    });

    let events = platform.drain_i2c_trace().expect("drain preemption trace");
    assert_eq!(accepted_count(&events, SimPic16Operation::Heartbeat), 1);
    assert_eq!(
        accepted_count(&events, SimPic16Operation::DisableVoltage),
        2
    );
    assert!(!admitted.is_current());
}

#[test]
fn batch_admission_is_round_robin_exclusive_and_batch_bound() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform
        .open_i2c_service(0)
        .expect("open simulated I2C service");
    let endpoint_55 = platform.pic16_endpoint(0, 0x55).expect("endpoint 55");
    let endpoint_56 = platform.pic16_endpoint(0, 0x56).expect("endpoint 56");
    let endpoint_57 = platform.pic16_endpoint(0, 0x57).expect("endpoint 57");

    let job = service
        .begin_pic16_admission([endpoint_57, endpoint_55, endpoint_56], 100)
        .expect("reserve batch admission");
    assert!(matches!(
        service.read_bytes(0x48, 1),
        Err(dcentrald_hal::HalError::I2cAdmissionBusy { .. })
    ));
    assert!(matches!(
        service.begin_pic16_admission(
            [platform
                .pic16_endpoint(0, 0x55)
                .expect("second start endpoint")],
            100
        ),
        Err(dcentrald_hal::HalError::I2cAdmissionBusy { .. })
    ));

    let mut events = Vec::new();
    drive_qualification(&platform, &mut events, 3);
    drain_until(&platform, &mut events, "all SET_VOLTAGE frames", |events| {
        accepted_count(events, SimPic16Operation::SetVoltage) == 3
    });
    assert_eq!(
        accepted_count(&events, SimPic16Operation::EnableVoltage),
        0,
        "ENABLE must wait for every SET plus the virtual settle gap"
    );

    platform
        .advance_i2c_time(Duration::from_millis(5))
        .expect("advance SET-to-ENABLE clock");
    drain_until(&platform, &mut events, "final heartbeat round", |events| {
        accepted_count(events, SimPic16Operation::EnableVoltage) == 3
            && accepted_count(events, SimPic16Operation::Heartbeat) == 18
    });

    let mut admitted = job.wait().expect("adopt admitted batch");
    assert_eq!(
        admitted
            .endpoints()
            .iter()
            .map(|endpoint| endpoint.address())
            .collect::<Vec<_>>(),
        [0x55, 0x56, 0x57]
    );
    assert!(admitted.is_current());
    assert!(admitted.endpoints().iter().all(|endpoint| {
        endpoint.evidence() == Pic16ApplicationEvidence::ApplicationModeUnknown
    }));

    let semantic: Vec<(u8, SimPic16Operation, u64)> = events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Pic16OperationAccepted {
                addr,
                operation,
                at_ms,
                ..
            } => Some((*addr, *operation, *at_ms)),
            _ => None,
        })
        .collect();
    let heartbeats: Vec<(u8, u64)> = semantic
        .iter()
        .filter_map(|(addr, operation, at_ms)| {
            (*operation == SimPic16Operation::Heartbeat).then_some((*addr, *at_ms))
        })
        .collect();
    assert_eq!(
        heartbeats,
        [
            (0x55, 0),
            (0x56, 0),
            (0x57, 0),
            (0x55, 1000),
            (0x56, 1000),
            (0x57, 1000),
            (0x55, 2000),
            (0x56, 2000),
            (0x57, 2000),
            (0x55, 3000),
            (0x56, 3000),
            (0x57, 3000),
            (0x55, 4000),
            (0x56, 4000),
            (0x57, 4000),
            (0x55, 4005),
            (0x56, 4005),
            (0x57, 4005),
        ]
    );
    let set_addresses: Vec<u8> = semantic
        .iter()
        .filter_map(|(addr, operation, _)| {
            (*operation == SimPic16Operation::SetVoltage).then_some(*addr)
        })
        .collect();
    let enable_addresses: Vec<u8> = semantic
        .iter()
        .filter_map(|(addr, operation, _)| {
            (*operation == SimPic16Operation::EnableVoltage).then_some(*addr)
        })
        .collect();
    assert_eq!(set_addresses, [0x55, 0x56, 0x57]);
    assert_eq!(enable_addresses, [0x55, 0x56, 0x57]);

    let _ = platform
        .drain_i2c_trace()
        .expect("clear admission trace before runtime round");
    let heartbeat = service
        .pic16_heartbeat_round(&mut admitted)
        .expect("batch-authorized heartbeat round");
    assert_eq!(heartbeat.addresses(), [0x55, 0x56, 0x57]);
    let runtime_addresses = platform
        .drain_i2c_trace()
        .expect("drain runtime heartbeat round")
        .into_iter()
        .filter_map(|event| match event {
            TraceEvent::Pic16OperationAccepted {
                addr,
                operation: SimPic16Operation::Heartbeat,
                ..
            } => Some(addr),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(runtime_addresses, [0x55, 0x56, 0x57]);
    assert_managed_pic16_superseded(service.read_bytes(0x55, 1), 0x55);
    assert_managed_pic16_superseded(
        service.set_voltage(0x55, dcentrald_hal::i2c::I2cPicFirmware::Unknown, 99),
        0x55,
    );
    assert_managed_pic16_superseded(
        service.disable_voltage(0x55, dcentrald_hal::i2c::I2cPicFirmware::Unknown),
        0x55,
    );
    assert!(matches!(
        service.begin_pic16_admission(
            [platform
                .pic16_endpoint(0, 0x55)
                .expect("overlapping admission endpoint")],
            101
        ),
        Err(dcentrald_hal::HalError::I2cAdmissionBusy { .. })
    ));
    let shutdown = admitted.safe_off_handle();
    let shutdown_clone = shutdown.clone();
    let safe_off = service
        .pic16_safe_off(&shutdown_clone)
        .expect("disable-only batch SafeOff");
    assert!(safe_off.all_disabled());
    assert_eq!(
        safe_off
            .endpoints()
            .iter()
            .map(|endpoint| endpoint.address())
            .collect::<Vec<_>>(),
        [0x57, 0x56, 0x55]
    );
    for address in 0x55..=0x57 {
        assert!(!platform
            .pic16_snapshot(0, address)
            .expect("batch SafeOff snapshot")
            .voltage_enabled());
    }
    let _ = platform
        .drain_i2c_trace()
        .expect("clear batch SafeOff trace before history checks");
    for address in 0x55..=0x57 {
        assert_managed_pic16_superseded(service.read_bytes(address, 1), address);
    }
    assert!(
        platform
            .drain_i2c_trace()
            .expect("drain multi-address history refusals")
            .is_empty(),
        "released multi-address history refusals must not reach the wire"
    );
    assert!(service.pic16_heartbeat_round(&mut admitted).is_err());
    let already_released = service
        .pic16_safe_off_admitted_batch(&mut admitted)
        .expect("released batch reports historical SafeOff");
    assert_eq!(
        already_released.disposition(),
        Pic16BatchSafeOffDisposition::AlreadyReleased
    );
    assert!(already_released.endpoints().is_empty());
}

#[test]
fn final_heartbeat_failure_compensates_every_activation_attempt() {
    let platform = SimPlatform::new(SimModel::S9);
    platform
        .schedule_pic16_fault(
            0,
            0x56,
            SimPic16Operation::Heartbeat,
            5,
            SimPic16Fault::TransportError,
        )
        .expect("schedule final heartbeat failure");
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let endpoint_55 = platform.pic16_endpoint(0, 0x55).expect("endpoint 55");
    let endpoint_56 = platform.pic16_endpoint(0, 0x56).expect("endpoint 56");
    let endpoint_57 = platform.pic16_endpoint(0, 0x57).expect("endpoint 57");
    let job = service
        .begin_pic16_admission([endpoint_55, endpoint_56, endpoint_57], 100)
        .expect("start admission");

    let mut events = Vec::new();
    drive_qualification(&platform, &mut events, 3);
    drain_until(&platform, &mut events, "SET phase", |events| {
        accepted_count(events, SimPic16Operation::SetVoltage) == 3
    });
    platform
        .advance_i2c_time(Duration::from_millis(5))
        .expect("advance enable clock");

    let failure = job.wait().expect_err("final heartbeat must fail admission");
    events.extend(platform.drain_i2c_trace().expect("drain final trace"));
    assert_eq!(failure.stage(), Pic16AdmissionStage::FinalHeartbeat);
    assert_eq!(failure.address(), Some(0x56));
    assert_eq!(
        failure
            .compensation()
            .iter()
            .map(|outcome| (outcome.address(), outcome.status()))
            .collect::<Vec<_>>(),
        [
            (0x57, &Pic16CompensationStatus::Disabled),
            (0x56, &Pic16CompensationStatus::Disabled),
            (0x55, &Pic16CompensationStatus::Disabled),
        ]
    );
    for address in 0x55..=0x57 {
        let snapshot = platform
            .pic16_snapshot(0, address)
            .expect("PIC16 endpoint snapshot");
        assert!(!snapshot.voltage_enabled(), "0x{address:02X} remained on");
    }
    let disables: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Pic16OperationAccepted {
                addr,
                operation: SimPic16Operation::DisableVoltage,
                ..
            } => Some(*addr),
            _ => None,
        })
        .collect();
    assert_eq!(disables, [0x57, 0x56, 0x55]);
}

#[test]
fn reserved_safe_off_preempts_post_jump_settle() {
    let platform = SimPlatform::new(SimModel::S9);
    platform
        .configure_pic16_raw_state(0, 0x55, 0xCC)
        .expect("put endpoint in exact bootloader state");
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let endpoint = platform.pic16_endpoint(0, 0x55).expect("endpoint 55");
    let job = service
        .begin_pic16_admission([endpoint], 100)
        .expect("start admission");

    let mut events = Vec::new();
    drain_until(&platform, &mut events, "fixed bootloader JUMP", |events| {
        accepted_count(events, SimPic16Operation::JumpFromLoader) == 1
    });
    assert_eq!(accepted_count(&events, SimPic16Operation::Heartbeat), 0);
    assert!(service
        .pic16_safe_off_active_batch()
        .expect("reserved SafeOff during settle")
        .expect("admission retains batch shutdown authority")
        .all_disabled());
    let failure = job.wait().expect_err("SafeOff must cancel admission");
    events.extend(platform.drain_i2c_trace().expect("drain cancelled trace"));
    assert!(matches!(
        failure.stage(),
        Pic16AdmissionStage::Cancellation | Pic16AdmissionStage::GenerationFence
    ));
    assert_eq!(accepted_count(&events, SimPic16Operation::Heartbeat), 0);
    assert_eq!(accepted_count(&events, SimPic16Operation::SetVoltage), 0);
    assert!(!platform
        .pic16_snapshot(0, 0x55)
        .expect("endpoint snapshot")
        .voltage_enabled());
}

#[test]
fn unadopted_batch_expires_and_is_compensated() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let endpoint = platform.pic16_endpoint(0, 0x55).expect("endpoint 55");
    let job = service
        .begin_pic16_admission([endpoint], 100)
        .expect("start admission");
    let mut events = Vec::new();
    drive_qualification(&platform, &mut events, 1);
    drain_until(&platform, &mut events, "SET frame", |events| {
        accepted_count(events, SimPic16Operation::SetVoltage) == 1
    });
    platform
        .advance_i2c_time(Duration::from_millis(5))
        .expect("advance enable clock");
    drain_until(
        &platform,
        &mut events,
        "provisional final heartbeat",
        |events| {
            accepted_count(events, SimPic16Operation::EnableVoltage) == 1
                && accepted_count(events, SimPic16Operation::Heartbeat) == 6
        },
    );

    platform
        .advance_i2c_time(Duration::from_secs(1))
        .expect("expire worker adoption deadline");
    drain_until(
        &platform,
        &mut events,
        "adoption-timeout compensation",
        |events| accepted_count(events, SimPic16Operation::DisableVoltage) == 1,
    );
    let failure = job
        .wait()
        .expect_err("unadopted provisional batch must be rolled back");
    assert_eq!(failure.stage(), Pic16AdmissionStage::BatchAdoption);
    assert_eq!(failure.compensation().len(), 1);
    assert_eq!(
        failure.compensation()[0].status(),
        &Pic16CompensationStatus::Disabled
    );
    assert!(!platform
        .pic16_snapshot(0, 0x55)
        .expect("expired-adoption snapshot")
        .voltage_enabled());
    assert_managed_pic16_superseded(service.read_bytes(0x55, 1), 0x55);
}

#[test]
fn one_unknown_qualification_failure_fails_closed_and_disables_every_requested_endpoint() {
    let platform = SimPlatform::new(SimModel::S9);
    platform
        .schedule_pic16_fault(
            0,
            0x56,
            SimPic16Operation::Heartbeat,
            2,
            SimPic16Fault::TransportError,
        )
        .expect("schedule third-round endpoint failure");
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let endpoints = [0x55, 0x56, 0x57]
        .map(|address| platform.pic16_endpoint(0, address).expect("PIC16 endpoint"));
    service
        .pic16_set_and_enable(&endpoints[1], 100)
        .expect("pre-enable endpoint whose cold state is not proven");
    assert!(platform
        .pic16_snapshot(0, 0x56)
        .expect("hot precondition snapshot")
        .voltage_enabled());
    let job = service
        .begin_pic16_admission(endpoints, 100)
        .expect("start admission");
    let mut events = Vec::new();
    drain_until(&platform, &mut events, "round 1", |events| {
        accepted_count(events, SimPic16Operation::Heartbeat) == 3
    });
    platform
        .advance_i2c_time(Duration::from_secs(1))
        .expect("advance round 2");
    drain_until(&platform, &mut events, "round 2", |events| {
        accepted_count(events, SimPic16Operation::Heartbeat) == 6
    });
    platform
        .advance_i2c_time(Duration::from_secs(1))
        .expect("advance failing round");
    drain_until(&platform, &mut events, "whole-batch cleanup", |events| {
        accepted_count(events, SimPic16Operation::DisableVoltage) == 3
    });
    let failure = job
        .wait()
        .expect_err("unknown rail state cannot produce a partial admitted batch");
    assert_eq!(failure.address(), Some(0x56));
    assert_eq!(failure.stage(), Pic16AdmissionStage::QualificationHeartbeat);
    assert_eq!(failure.compensation().len(), 3);
    assert!(failure
        .compensation()
        .iter()
        .all(|endpoint| endpoint.status() == &Pic16CompensationStatus::Disabled));
    assert_eq!(accepted_count(&events, SimPic16Operation::SetVoltage), 1);
    assert_eq!(accepted_count(&events, SimPic16Operation::EnableVoltage), 1);
    for address in 0x55..=0x57 {
        assert!(!platform
            .pic16_snapshot(0, address)
            .expect("fail-closed snapshot")
            .voltage_enabled());
    }
}

#[test]
fn set_failure_compensates_whole_batch_and_retries_disable_once() {
    let platform = SimPlatform::new(SimModel::S9);
    platform
        .schedule_pic16_fault(
            0,
            0x55,
            SimPic16Operation::SetVoltage,
            0,
            SimPic16Fault::TransportError,
        )
        .expect("schedule SET failure");
    for _ in 0..2 {
        platform
            .schedule_pic16_fault(
                0,
                0x56,
                SimPic16Operation::DisableVoltage,
                0,
                SimPic16Fault::TransportError,
            )
            .expect("schedule disable failure");
    }
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let endpoints = [0x55, 0x56, 0x57]
        .map(|address| platform.pic16_endpoint(0, address).expect("PIC16 endpoint"));
    let job = service
        .begin_pic16_admission(endpoints, 100)
        .expect("start admission");
    let mut events = Vec::new();
    drive_qualification(&platform, &mut events, 3);
    let failure = job.wait().expect_err("SET failure must abort batch");
    events.extend(platform.drain_i2c_trace().expect("drain rollback trace"));
    assert_eq!(failure.stage(), Pic16AdmissionStage::SetVoltage);
    assert_eq!(failure.address(), Some(0x55));
    assert_eq!(
        failure
            .compensation()
            .iter()
            .map(|outcome| outcome.address())
            .collect::<Vec<_>>(),
        [0x57, 0x56, 0x55]
    );
    assert!(matches!(
        failure.compensation()[1].status(),
        Pic16CompensationStatus::OutcomeUnknown { .. }
    ));
    assert!(service.terminal_safe_off_is_latched());
    let accepted_disables: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Pic16OperationAccepted {
                addr,
                operation: SimPic16Operation::DisableVoltage,
                ..
            } => Some(*addr),
            _ => None,
        })
        .collect();
    assert_eq!(accepted_disables, [0x57, 0x55]);

    let recovery = service
        .pic16_safe_off_active_batch()
        .expect("retry retained pre-adoption batch SafeOff")
        .expect("failed admission must retain shutdown authority");
    assert!(recovery.all_disabled());
    assert!(service
        .pic16_safe_off_active_batch()
        .expect("inspect released recovery batch")
        .is_none());

    drop(service);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match platform.open_i2c_service(0) {
            Ok(replacement) => {
                drop(replacement);
                break;
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => panic!("proven recovery remained quarantined: {error}"),
        }
    }
}

#[test]
fn post_jump_read_waits_for_exact_virtual_settle_boundary() {
    let platform = SimPlatform::new(SimModel::S9);
    platform
        .configure_pic16_raw_state(0, 0x55, 0xCC)
        .expect("configure bootloader state");
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let endpoint = platform.pic16_endpoint(0, 0x55).expect("endpoint 55");
    let job = service
        .begin_pic16_admission([endpoint], 100)
        .expect("start admission");
    let mut events = Vec::new();
    drain_until(&platform, &mut events, "JUMP", |events| {
        accepted_count(events, SimPic16Operation::JumpFromLoader) == 1
    });
    assert_eq!(accepted_count(&events, SimPic16Operation::RawRead), 1);
    platform
        .advance_i2c_time(Duration::from_millis(499))
        .expect("advance below settle boundary");
    std::thread::sleep(Duration::from_millis(15));
    events.extend(
        platform
            .drain_i2c_trace()
            .expect("drain pre-boundary trace"),
    );
    assert_eq!(accepted_count(&events, SimPic16Operation::RawRead), 1);
    assert_eq!(accepted_count(&events, SimPic16Operation::Heartbeat), 0);

    platform
        .advance_i2c_time(Duration::from_millis(1))
        .expect("reach settle boundary");
    drain_until(&platform, &mut events, "post-JUMP observation", |events| {
        accepted_count(events, SimPic16Operation::RawRead) == 2
    });
    assert!(service
        .pic16_safe_off_active_batch()
        .expect("cancel remaining qualification")
        .expect("admission retains batch shutdown authority")
        .all_disabled());
    assert!(job.wait().is_err());
}

#[test]
fn one_virtual_fabric_cannot_have_two_service_owners() {
    let platform = SimPlatform::new(SimModel::S9);
    let first = platform
        .open_i2c_service(0)
        .expect("first serialized service owner");
    assert!(
        platform.open_i2c_service(0).is_err(),
        "duplicate worker must not receive an independent safety authority"
    );
    drop(first);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match platform.open_i2c_service(0) {
            Ok(replacement) => {
                drop(replacement);
                break;
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("service registry did not release after worker exit: {error}"),
        }
    }
}

#[test]
fn generic_terminal_lifecycle_does_not_quarantine_clean_replacement() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    service.latch_terminal_safe_off();
    drop(service);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match platform.open_i2c_service(0) {
            Ok(replacement) => {
                drop(replacement);
                break;
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("generic terminal lifecycle quarantined replacement: {error}"),
        }
    }
}

#[test]
fn provisional_batch_safe_off_is_reconciled_without_false_quarantine() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let endpoint = platform.pic16_endpoint(0, 0x55).expect("endpoint 55");
    let job = service
        .begin_pic16_admission([endpoint], 100)
        .expect("start provisional admission");
    let mut events = Vec::new();
    drive_qualification(&platform, &mut events, 1);
    drain_until(&platform, &mut events, "provisional SET", |events| {
        accepted_count(events, SimPic16Operation::SetVoltage) == 1
    });
    platform
        .advance_i2c_time(Duration::from_millis(5))
        .expect("advance provisional enable gap");
    drain_until(
        &platform,
        &mut events,
        "provisional final heartbeat",
        |events| {
            accepted_count(events, SimPic16Operation::EnableVoltage) == 1
                && accepted_count(events, SimPic16Operation::Heartbeat) == 6
        },
    );
    platform
        .schedule_pic16_fault(
            0,
            0x55,
            SimPic16Operation::DisableVoltage,
            1,
            SimPic16Fault::TransportError,
        )
        .expect("arm failure after the proven batch SafeOff");
    platform
        .schedule_pic16_fault(
            0,
            0x55,
            SimPic16Operation::DisableVoltage,
            0,
            SimPic16Fault::TransportError,
        )
        .expect("arm retry failure after the proven batch SafeOff");

    let deadline = Instant::now() + Duration::from_secs(2);
    let safe_off = loop {
        match service
            .pic16_safe_off_active_batch()
            .expect("inspect retained provisional batch")
        {
            Some(outcome) => break outcome,
            None if Instant::now() < deadline => std::thread::yield_now(),
            None => panic!("provisional batch was not published"),
        }
    };
    assert!(safe_off.all_disabled());

    let failure = job
        .wait()
        .expect_err("concurrent SafeOff must cancel provisional adoption");
    assert!(!failure.cleanup_pending());
    assert!(failure
        .compensation()
        .iter()
        .all(|outcome| outcome.status() == &Pic16CompensationStatus::Disabled));
    assert!(!platform
        .pic16_snapshot(0, 0x55)
        .expect("provisional SafeOff snapshot")
        .voltage_enabled());

    drop(service);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match platform.open_i2c_service(0) {
            Ok(replacement) => {
                drop(replacement);
                break;
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => panic!("proven provisional SafeOff quarantined replacement: {error}"),
        }
    }
}

#[test]
fn stale_released_epoch_cannot_claim_a_newer_active_batch_is_off() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let first = admit_single_pic16(&platform, &service, 0x55);
    let stale = first.safe_off_handle();
    assert!(service
        .pic16_safe_off(&stale)
        .expect("release first epoch")
        .all_disabled());

    let mut second = admit_single_pic16(&platform, &service, 0x55);
    assert!(matches!(
        service.pic16_safe_off(&stale),
        Err(dcentrald_hal::HalError::I2cSafetySuperseded { .. })
    ));
    assert!(platform
        .pic16_snapshot(0, 0x55)
        .expect("newer epoch snapshot")
        .voltage_enabled());
    assert!(service
        .pic16_safe_off_admitted_batch(&mut second)
        .expect("release newer epoch")
        .all_disabled());
}

#[test]
fn dropping_admitted_batch_safes_off_while_service_remains_live() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let legacy_endpoint = platform
        .pic16_endpoint(0, 0x55)
        .expect("retained legacy endpoint capability");
    let admitted = admit_single_pic16(&platform, &service, 0x55);
    let _ = platform
        .drain_i2c_trace()
        .expect("clear admission trace before owner drop");

    drop(admitted);

    let mut events = Vec::new();
    drain_until(
        &platform,
        &mut events,
        "drop-owned whole-batch SafeOff",
        |events| accepted_count(events, SimPic16Operation::DisableVoltage) == 1,
    );
    assert!(!platform
        .pic16_snapshot(0, 0x55)
        .expect("drop SafeOff snapshot")
        .voltage_enabled());
    assert_eq!(service.pic16_active_batch_addresses(), None);

    let _ = platform
        .drain_i2c_trace()
        .expect("clear drop SafeOff trace before raw fence checks");
    assert_managed_pic16_superseded(service.read_bytes(0x55, 1), 0x55);
    assert_managed_pic16_superseded(
        service.heartbeat(0x55, dcentrald_hal::i2c::I2cPicFirmware::Unknown),
        0x55,
    );
    assert_managed_pic16_superseded(
        service.set_voltage(0x55, dcentrald_hal::i2c::I2cPicFirmware::Unknown, 99),
        0x55,
    );
    assert_managed_pic16_superseded(
        service.disable_voltage(0x55, dcentrald_hal::i2c::I2cPicFirmware::Unknown),
        0x55,
    );
    assert_managed_pic16_superseded(service.pic16_read_raw_exact(&legacy_endpoint), 0x55);
    assert_managed_pic16_superseded(service.pic16_heartbeat(&legacy_endpoint), 0x55);
    assert_managed_pic16_superseded(
        service.pic16_jump_if_exact_bootloader(&legacy_endpoint),
        0x55,
    );
    assert_managed_pic16_superseded(service.pic16_set_and_enable(&legacy_endpoint, 99), 0x55);
    assert!(
        platform
            .drain_i2c_trace()
            .expect("drain refused raw operations")
            .is_empty(),
        "post-release raw refusals must not reach the virtual wire"
    );
    service
        .read_bytes(0x56, 1)
        .expect("an unmanaged sibling address remains available");

    let mut replacement = admit_single_pic16(&platform, &service, 0x55);
    assert!(service
        .pic16_safe_off_admitted_batch(&mut replacement)
        .expect("release post-drop replacement batch")
        .all_disabled());
}

#[test]
fn worker_disconnect_safes_off_retained_batch_before_registry_release() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let outcome = admit_single_pic16(&platform, &service, 0x55);
    drop(outcome);
    drop(service);

    let deadline = Instant::now() + Duration::from_secs(2);
    let replacement = loop {
        match platform.open_i2c_service(0) {
            Ok(replacement) => break replacement,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("clean worker teardown did not release service: {error}"),
        }
    };
    assert!(!platform
        .pic16_snapshot(0, 0x55)
        .expect("disconnect SafeOff snapshot")
        .voltage_enabled());
    replacement
        .read_bytes(0x55, 1)
        .expect("clean replacement service starts with empty managed history");
    drop(replacement);
}

#[test]
fn unresolved_disconnect_quarantines_virtual_fabric_replacement() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform.open_i2c_service(0).expect("sim I2C service");
    let outcome = admit_single_pic16(&platform, &service, 0x55);
    for ordinal in 1..=4 {
        platform
            .schedule_pic16_fault(
                0,
                0x55,
                SimPic16Operation::DisableVoltage,
                0,
                SimPic16Fault::TransportError,
            )
            .unwrap_or_else(|error| {
                panic!("schedule unresolved SafeOff failure {ordinal}: {error}")
            });
    }
    let _ = platform
        .drain_i2c_trace()
        .expect("clear pre-disconnect trace");
    drop(outcome);
    drop(service);

    let mut events = Vec::new();
    drain_until(
        &platform,
        &mut events,
        "drop and disconnect SafeOff retries",
        |events| {
            events
                .iter()
                .filter(|event| matches!(event, TraceEvent::I2cRecovery { .. }))
                .count()
                == 2
        },
    );
    std::thread::sleep(Duration::from_millis(20));
    let error = match platform.open_i2c_service(0) {
        Ok(_) => panic!("unresolved live batch must quarantine replacement"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("quarantined"), "{error}");
    assert!(platform
        .pic16_snapshot(0, 0x55)
        .expect("unresolved disconnect snapshot")
        .voltage_enabled());
}
