#![cfg(feature = "sim-hal")]

use dcentrald_asic::drivers::ChipRegistry;
use dcentrald_hal::fpga_chain::{FpgaChain, BAUD_REG_115200};
use dcentrald_hal::platform::sim::{SimModel, TraceEvent};

/// First real-driver simulator proof on the staged path to T2.
///
/// This is intentionally not called a golden/T3 test: it proves the production
/// registry's BM1398 driver can execute its full init sequence against a
/// device-free `FpgaChain`, but it does not yet compare against a provenance
/// vector or submit a share through MockV1Pool.
#[test]
fn production_bm1398_driver_completes_s19pro_init_on_simulated_fpga() {
    let registry = ChipRegistry::production();
    let driver = registry.detect(0x1398).expect("production BM1398 driver");
    let mut chain =
        FpgaChain::open_sim_for_model(1, SimModel::S19Pro).expect("device-free S19 Pro FPGA chain");

    driver
        .init_chain(&mut chain, 114, 650)
        .expect("BM1398 init_chain must complete on simulator");

    let trace = chain.drain_sim_trace().expect("driver trace");
    let command_words = trace
        .iter()
        .filter(|event| matches!(event, TraceEvent::Command { .. }))
        .count();
    assert!(
        command_words >= 20,
        "full BM1398 init should emit a substantial ordered command sequence; got {command_words} words"
    );
    assert!(trace.iter().any(|event| matches!(
        event,
        TraceEvent::BaudChanged { baud, .. }
            if *baud == FpgaChain::baud_from_divisor(BAUD_REG_115200)
    )));
}
