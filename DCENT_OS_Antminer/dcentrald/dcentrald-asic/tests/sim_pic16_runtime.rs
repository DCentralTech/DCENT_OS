#![cfg(feature = "sim-hal")]

use dcentrald_asic::pic::{PicController, PicFirmware};
use dcentrald_hal::platform::sim::{SimModel, SimPlatform, TraceEvent};
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
