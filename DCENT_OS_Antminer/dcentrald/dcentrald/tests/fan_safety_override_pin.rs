//! Host-runnable pin for the daemon emergency fan override path.
//!
//! `dcentrald` is a binary crate, so `src/chain.rs` unit tests are not reached
//! by `cargo test -p dcentrald --lib`. This integration test pins the same
//! contract without touching hardware: the shared HAL cap remains PWM 30, and
//! the daemon override path continues to command `FAN_PWM_SAFETY` instead of the
//! hardware max.

const CHAIN_RS: &str = include_str!("../src/chain.rs");

#[test]
fn fan_safety_override_commands_pwm_30_not_hardware_max() {
    assert_eq!(
        dcentrald_hal::fan::PWM_SAFETY_MAX,
        30,
        "fan-never-blast safety cap must stay PWM 30"
    );
    assert!(
        dcentrald_hal::fan::PWM_SAFETY_MAX < dcentrald_hal::fan::PWM_MAX,
        "fan safety cap must stay below the hardware max"
    );
    assert!(
        CHAIN_RS.contains("pub const FAN_PWM_SAFETY: u8 = dcentrald_hal::fan::PWM_SAFETY_MAX;"),
        "daemon FAN_PWM_SAFETY alias must keep using the shared HAL safety cap"
    );
    assert!(
        CHAIN_RS.contains("fan.set_speed(FAN_PWM_SAFETY);"),
        "fan_safety_override must command FAN_PWM_SAFETY, not FAN_PWM_MAX"
    );
}
