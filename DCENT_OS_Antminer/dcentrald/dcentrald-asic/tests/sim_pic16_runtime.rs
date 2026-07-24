#![cfg(feature = "sim-hal")]

use dcentrald_asic::pic::{Pic16EndpointSession, PicController, PicFirmware};
use dcentrald_hal::platform::sim::{
    SimModel, SimPic16Fault, SimPic16Operation, SimPlatform, TraceEvent,
};
use dcentrald_hal::platform::Platform;

fn production_runtime_trace(firmware: PicFirmware) -> Vec<u8> {
    let platform = SimPlatform::new(SimModel::S9);
    let mut bus = platform.open_i2c(0).expect("open simulated S9 I2C bus");
    let mut pic = PicController::new_with_firmware(&mut bus, 0x55, firmware);

    pic.set_voltage(100).expect("set PIC16 voltage");
    pic.enable_voltage().expect("enable PIC16 voltage");
    pic.send_heartbeat().expect("send PIC16 heartbeat");
    pic.disable_voltage().expect("disable PIC16 voltage");
    drop(pic);

    platform
        .drain_i2c_trace()
        .expect("drain simulated I2C trace")
        .into_iter()
        .filter_map(|event| match event {
            TraceEvent::I2cWrite {
                bus: 0,
                addr: 0x55,
                bytes,
            } => Some(bytes),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn stock_and_braiins_pic_controllers_share_the_documented_runtime_grammar() {
    let expected = [
        0x55, 0xAA, 0x10, 100, // SET_VOLTAGE
        0x55, 0xAA, 0x15, 0x01, // ENABLE_VOLTAGE
        0x55, 0xAA, 0x16, // HEARTBEAT
        0x55, 0xAA, 0x15, 0x00, // DISABLE_VOLTAGE
    ];

    assert_eq!(production_runtime_trace(PicFirmware::Stock(0x5A)), expected);
    assert_eq!(production_runtime_trace(PicFirmware::BraiinsOs), expected);
}

#[test]
fn service_cold_boot_requires_five_heartbeats_before_one_set_and_enable() {
    let platform = SimPlatform::new(SimModel::S9);
    let service = platform
        .open_i2c_service(0)
        .expect("open simulated serialized I2C service");
    let endpoint = platform
        .pic16_endpoint(0, 0x55)
        .expect("issue simulated PIC16 endpoint");
    let session = Pic16EndpointSession::new(service, endpoint).expect("bind endpoint session");
    let mut pic = session.service();

    pic.cold_boot_init(100)
        .expect("admit simulated app-mode PIC16");

    let wire: Vec<u8> = platform
        .drain_i2c_trace()
        .expect("drain cold-boot trace")
        .into_iter()
        .filter_map(|event| match event {
            TraceEvent::I2cWrite {
                bus: 0,
                addr: 0x55,
                bytes,
            } => Some(bytes),
            _ => None,
        })
        .flatten()
        .collect();

    let mut expected = Vec::new();
    for _ in 0..5 {
        expected.extend_from_slice(&[0x55, 0xAA, 0x16]);
    }
    expected.extend_from_slice(&[0x55, 0xAA, 0x10, 100]);
    expected.extend_from_slice(&[0x55, 0xAA, 0x15, 0x01]);
    expected.extend_from_slice(&[0x55, 0xAA, 0x16]);

    assert_eq!(wire, expected);
    assert!(platform
        .i2c_voltage_enabled()
        .expect("simulated rail state"));
}

#[test]
fn service_cold_boot_cannot_activate_after_the_fifth_heartbeat_fails() {
    let platform = SimPlatform::new(SimModel::S9);
    platform
        .schedule_pic16_fault(
            0,
            0x55,
            SimPic16Operation::Heartbeat,
            4,
            SimPic16Fault::TransportError,
        )
        .expect("schedule fifth heartbeat fault");
    platform
        .schedule_pic16_fault(
            0,
            0x55,
            SimPic16Operation::Heartbeat,
            0,
            SimPic16Fault::TransportError,
        )
        .expect("schedule retry fault");
    let service = platform
        .open_i2c_service(0)
        .expect("open simulated serialized I2C service");
    let endpoint = platform
        .pic16_endpoint(0, 0x55)
        .expect("issue simulated PIC16 endpoint");
    let session = Pic16EndpointSession::new(service, endpoint).expect("bind endpoint session");
    let mut pic = session.service();

    assert!(pic.cold_boot_init(100).is_err());
    let snapshot = platform
        .pic16_snapshot(0, 0x55)
        .expect("failed endpoint snapshot");
    assert!(!snapshot.voltage_enabled());
    assert_eq!(snapshot.voltage_pic(), None);
    assert_eq!(snapshot.heartbeat_count(), 4);

    let accepted = platform
        .drain_i2c_trace()
        .expect("accepted operation trace");
    assert_eq!(
        accepted
            .iter()
            .filter(|event| matches!(
                event,
                TraceEvent::Pic16OperationAccepted {
                    operation: SimPic16Operation::Heartbeat,
                    ..
                }
            ))
            .count(),
        4
    );
    assert!(!accepted.iter().any(|event| matches!(
        event,
        TraceEvent::Pic16OperationAccepted {
            operation: SimPic16Operation::SetVoltage | SimPic16Operation::EnableVoltage,
            ..
        }
    )));
}

#[test]
fn service_cold_boot_rejects_unknown_raw_state_without_mutation() {
    let platform = SimPlatform::new(SimModel::S9);
    platform
        .configure_pic16_raw_state(0, 0x55, 0x42)
        .expect("configure unknown raw state");
    let service = platform
        .open_i2c_service(0)
        .expect("open simulated serialized I2C service");
    let endpoint = platform
        .pic16_endpoint(0, 0x55)
        .expect("issue simulated PIC16 endpoint");
    let session = Pic16EndpointSession::new(service, endpoint).expect("bind endpoint session");
    let mut pic = session.service();

    assert!(pic.cold_boot_init(100).is_err());
    let snapshot = platform
        .pic16_snapshot(0, 0x55)
        .expect("rejected endpoint snapshot");
    assert_eq!(snapshot.raw_state(), 0x42);
    assert!(!snapshot.voltage_enabled());
    assert_eq!(snapshot.voltage_pic(), None);
    assert_eq!(snapshot.heartbeat_count(), 0);
    assert!(!platform
        .drain_i2c_trace()
        .expect("operation trace")
        .into_iter()
        .any(|event| matches!(
            event,
            TraceEvent::Pic16OperationAccepted {
                operation: SimPic16Operation::JumpFromLoader
                    | SimPic16Operation::Heartbeat
                    | SimPic16Operation::SetVoltage
                    | SimPic16Operation::EnableVoltage,
                ..
            }
        )));
}
