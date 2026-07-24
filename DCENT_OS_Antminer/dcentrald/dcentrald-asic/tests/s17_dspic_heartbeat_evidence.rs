//! Cross-crate contract for the held S17 factory-jig heartbeat evidence.
//!
//! This pins evidence only. It must not promote native S17 runtime admission:
//! the held host binary proves its own cadence and wire predicate, but not the
//! dsPIC hardware watchdog timeout or electrical safety margin.

use dcentrald_asic::dspic::{CMD_HEARTBEAT, DSPIC_PREAMBLE, HEARTBEAT_INTERVAL_MS};
use dcentrald_silicon_profiles::pic_heartbeat::{
    pic_heartbeat_config, s17_stock_heartbeat_acknowledged, PicFw, Platform,
    S17_STOCK_HEARTBEAT_FRAME, S17_STOCK_OBSERVED_VENDOR_CADENCE_MS,
};

#[test]
fn driver_fallback_matches_observed_s17_vendor_cadence_not_runtime_policy() {
    let profile = pic_heartbeat_config(Platform::S17Am1, PicFw::Dspic33epHealthy);

    assert_eq!(
        HEARTBEAT_INTERVAL_MS,
        u64::from(S17_STOCK_OBSERVED_VENDOR_CADENCE_MS)
    );
    assert_eq!(
        profile.observed_vendor_cadence_ms,
        Some(S17_STOCK_OBSERVED_VENDOR_CADENCE_MS)
    );
    assert_eq!(profile.interval_ms, 0);
    assert_eq!(profile.watchdog_timeout_ms, 0);
    assert!(!profile.needs_heartbeat_thread());
    assert!(!profile.voltage_allowed());
}

#[test]
fn driver_protocol_constants_reconstruct_the_held_s17_frame() {
    let checksum = 0x04u8.wrapping_add(CMD_HEARTBEAT);
    let reconstructed = [
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        0x04,
        CMD_HEARTBEAT,
        0x00,
        checksum,
    ];

    assert_eq!(reconstructed, S17_STOCK_HEARTBEAT_FRAME);
    assert!(s17_stock_heartbeat_acknowledged(&[
        0x00,
        CMD_HEARTBEAT,
        0x01,
        0x00,
        0x00,
        0x00,
    ]));
    assert!(!s17_stock_heartbeat_acknowledged(&[
        0x00,
        CMD_HEARTBEAT,
        0x00,
        0x00,
        0x00,
        0x00,
    ]));
}
