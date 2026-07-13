#![cfg(feature = "sim-hal")]

use dcentrald_asic::dspic::{DspicFirmware, DspicService};
use dcentrald_hal::platform::sim::{SimModel, SimPlatform};

#[test]
fn typed_fw89_safe_off_cuts_an_enabled_simulated_rail() {
    let platform = SimPlatform::new(SimModel::S19Pro);
    let service = platform.open_i2c_service(0).unwrap();

    service
        .write_bytes(0x20, &[0x55, 0xAA, 0x15, 0x01])
        .unwrap();
    assert!(platform.i2c_voltage_enabled().unwrap());

    service.latch_terminal_safe_off();
    let mut controller =
        DspicService::new_with_firmware(service.clone(), 0x20, DspicFirmware::Fw89);
    controller.disable_voltage().unwrap();

    let enabled = platform.i2c_voltage_enabled().unwrap();
    assert!(
        !enabled,
        "typed dsPIC safe-off did not cut simulated rail; trace={:?}",
        platform.drain_i2c_trace().unwrap()
    );
    assert!(service.terminal_safe_off_is_latched());
}
