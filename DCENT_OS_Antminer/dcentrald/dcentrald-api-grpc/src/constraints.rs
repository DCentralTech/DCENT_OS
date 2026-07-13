//! TunerConstraints builder — the one real (non-stub) handler in the
//! Supremacy S5.1 scaffold.
//!
//! Pulls frequency envelope from `dcentrald-silicon-profiles` (BM1362 default)
//! and clamps voltage / fan envelopes via the LOAD-BEARING SUPREMACY rules:
//!
//! - **am2 voltage cap (14500 mV)** — see
//!   . Never raise this without
//!   a hardware-acquisition + multi-agent review.
//! - **home-mode fan cap (PWM 30)** — see `rust-firmware.md` rule "NEVER allow
//!   fans above PWM 30 for home mining". `MODE_HOME` enforces the cap; other
//!   modes inherit a softer 100-PWM ceiling but pass through `home_max_pwm`
//!   for client-side visibility.
//!
//! The handler does NOT consult a live HAL — it's deterministic data derived
//! from silicon-profile tables, so it stays unit-testable on every host.

use crate::dcent::v1::{
    FanEnvelope, FrequencyBand, OperatingMode, TunerConstraints, VoltageEnvelope,
};

/// Absolute SUPREMACY voltage ceiling for am2 (BM1362 family). Mirrored from
///  + the dcentrald
/// `[autotune.power_target]` Rust PI controller.
pub const VOLTAGE_MAX_MV_AM2: u32 = 14_500;

/// SUPREMACY home-mode fan cap. Mirrored from `rust-firmware.md` ("NEVER
/// allow fans above PWM 30 for home mining"). Applied unconditionally when
/// `mode = MODE_HOME`. Other modes report 100 (or the platform-supported
/// ceiling) for client-side awareness.
pub const HOME_FAN_PWM_MAX: u32 = 30;

/// Non-home fan ceiling. Conservative software cap — actual hardware tach
/// + thermal supervisor still enforces stricter limits.
pub const NON_HOME_FAN_PWM_MAX: u32 = 100;

/// Voltage floor — below this the chips refuse to mine. Pulled from the
/// BM1362 silicon-profile table's lowest live-confirmed row (Step -16 / -13
/// share the 11.880 V silicon voltage floor).
pub const VOLTAGE_MIN_MV: u32 = 11_880;

/// Default BM1362 frequency band when `silicon-profiles` is queried with
/// no chip-family override. Derived from the 21-row table:
/// min 145 MHz (Step -16) -> max 645 MHz (Step +4), 25 MHz cadence.
pub const DEFAULT_BM1362_FREQ_MIN_MHZ: u32 = 145;
pub const DEFAULT_BM1362_FREQ_MAX_MHZ: u32 = 645;
pub const DEFAULT_BM1362_FREQ_STEP_MHZ: u32 = 25;

/// Build the canonical `TunerConstraints` reply for the given mode.
///
/// `home_mode` is a convenience boolean used by the caller — when wired
/// by daemon startup, it comes from
/// `OperatingMode::from_config_str(config.mode.active).is_home()` so config
/// aliases such as `heater` receive the Home fan cap. The fan envelope's
/// `max_pwm` is clamped to `HOME_FAN_PWM_MAX` when true.
pub fn build_bm1362_constraints(home_mode: bool) -> TunerConstraints {
    let mode = if home_mode {
        OperatingMode::ModeHome
    } else {
        OperatingMode::ModeStandard
    };
    build_constraints(home_mode, mode, bm1362_freq_band())
}

/// Mode-aware builder so the caller can override the reported `mode` field
/// (e.g. echo back the operator's MODE_PERFORMANCE request while still
/// applying the home cap because `home_mode=true`).
pub fn build_constraints(
    home_mode: bool,
    mode: OperatingMode,
    freq: FrequencyBand,
) -> TunerConstraints {
    let fan_max = if home_mode {
        HOME_FAN_PWM_MAX
    } else {
        NON_HOME_FAN_PWM_MAX
    };
    TunerConstraints {
        chip_family: "bm1362".to_string(),
        frequency_band: Some(freq),
        voltage_envelope: Some(VoltageEnvelope {
            min_mv: VOLTAGE_MIN_MV,
            max_mv: VOLTAGE_MAX_MV_AM2,
        }),
        fan_envelope: Some(FanEnvelope {
            min_pwm: 0,
            max_pwm: fan_max,
        }),
        mode: mode as i32,
        source: "silicon-profiles".to_string(),
    }
}

/// CE-122: source string for a per-family reply where only the frequency band
/// is derived from a validated table — the voltage envelope is intentionally
/// omitted (`None`) because we do not have a validated mV envelope for that
/// family, and advertising the am2 14500 mV cap for a non-BM1362 chip would be
/// dishonest. Write-path enforcement stays in the REST clamps.
pub const SOURCE_FREQ_ONLY: &str = "silicon-profiles:freq-band-only-voltage-envelope-not-validated";

/// CE-122: platform/chip-aware constraints. Non-BM1362 families get their
/// validated frequency band + the universal home fan cap, but NO voltage
/// envelope (honest: the 14500 mV am2 cap is BM1362-only). Any unknown/empty
/// family falls back to the byte-identical BM1362 default (backward-compatible).
pub fn build_constraints_for_chip(chip_family: &str, home_mode: bool) -> TunerConstraints {
    let family = chip_family.trim().to_ascii_lowercase();
    let table: Option<&'static dcentrald_silicon_profiles::SiliconTable> = match family.as_str() {
        "bm1387" => Some(&dcentrald_silicon_profiles::bm1387::BM1387_TABLE),
        "bm1397" => Some(&dcentrald_silicon_profiles::bm1397::BM1397_TABLE),
        "bm1398" => Some(&dcentrald_silicon_profiles::bm1398::BM1398_TABLE),
        "bm1366" => Some(&dcentrald_silicon_profiles::bm1366::BM1366_TABLE),
        "bm1368" => Some(&dcentrald_silicon_profiles::bm1368::BM1368_TABLE),
        "bm1370" => Some(&dcentrald_silicon_profiles::bm1370::BM1370_TABLE),
        // "bm1362" and every unknown/empty family → the pinned BM1362 default.
        _ => None,
    };
    match table {
        Some(t) => build_family_freq_only_constraints(&family, t, home_mode),
        None => build_bm1362_constraints(home_mode),
    }
}

fn build_family_freq_only_constraints(
    family: &str,
    table: &'static dcentrald_silicon_profiles::SiliconTable,
    home_mode: bool,
) -> TunerConstraints {
    let freqs: Vec<u32> = table.profiles.iter().map(|p| p.freq_mhz).collect();
    let min = freqs
        .iter()
        .copied()
        .min()
        .unwrap_or(DEFAULT_BM1362_FREQ_MIN_MHZ);
    let max = freqs
        .iter()
        .copied()
        .max()
        .unwrap_or(DEFAULT_BM1362_FREQ_MAX_MHZ);
    let step = freq_step_from_freqs(&freqs);
    let mode = if home_mode {
        OperatingMode::ModeHome
    } else {
        OperatingMode::ModeStandard
    };
    let fan_max = if home_mode {
        HOME_FAN_PWM_MAX
    } else {
        NON_HOME_FAN_PWM_MAX
    };
    TunerConstraints {
        chip_family: family.to_string(),
        frequency_band: Some(FrequencyBand {
            min_mhz: min,
            max_mhz: max,
            step_mhz: step,
        }),
        // HONEST: only bm1362 advertises the validated 14500 mV envelope.
        voltage_envelope: None,
        fan_envelope: Some(FanEnvelope {
            min_pwm: 0,
            max_pwm: fan_max,
        }),
        mode: mode as i32,
        source: SOURCE_FREQ_ONLY.to_string(),
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// GCD of consecutive frequency deltas → the coarsest common step. Falls back to
/// the BM1362 default step for degenerate (0/1-row) tables.
fn freq_step_from_freqs(freqs: &[u32]) -> u32 {
    let mut sorted: Vec<u32> = freqs.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() < 2 {
        return DEFAULT_BM1362_FREQ_STEP_MHZ;
    }
    let mut step = 0u32;
    for w in sorted.windows(2) {
        step = gcd(step, w[1] - w[0]);
    }
    if step == 0 {
        DEFAULT_BM1362_FREQ_STEP_MHZ
    } else {
        step
    }
}

/// Default BM1362 frequency band derived from the silicon-profile table.
/// Kept as a pure helper so the `bm1362` integration is verified in the unit
/// tests without pulling the whole table into the proto reply path.
pub fn bm1362_freq_band() -> FrequencyBand {
    let table = dcentrald_silicon_profiles::bm1362::BM1362_PROFILES;
    let min = table
        .iter()
        .map(|p| p.freq_mhz)
        .min()
        .unwrap_or(DEFAULT_BM1362_FREQ_MIN_MHZ);
    let max = table
        .iter()
        .map(|p| p.freq_mhz)
        .max()
        .unwrap_or(DEFAULT_BM1362_FREQ_MAX_MHZ);
    FrequencyBand {
        min_mhz: min,
        max_mhz: max,
        step_mhz: DEFAULT_BM1362_FREQ_STEP_MHZ,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voltage_cap_never_exceeds_14500_am2() {
        let c = build_bm1362_constraints(false);
        let envelope = c.voltage_envelope.expect("voltage envelope present");
        assert!(
            envelope.max_mv <= VOLTAGE_MAX_MV_AM2,
            "voltage cap regressed past 14500 mV — load-bearing SUPREMACY clamp",
        );
        assert_eq!(envelope.max_mv, VOLTAGE_MAX_MV_AM2);
    }

    #[test]
    fn voltage_cap_also_applies_in_home_mode() {
        let c = build_bm1362_constraints(true);
        let envelope = c.voltage_envelope.expect("voltage envelope present");
        assert!(envelope.max_mv <= VOLTAGE_MAX_MV_AM2);
    }

    #[test]
    fn home_mode_caps_fan_pwm_at_30() {
        let c = build_bm1362_constraints(true);
        let fan = c.fan_envelope.expect("fan envelope present");
        assert!(
            fan.max_pwm <= HOME_FAN_PWM_MAX,
            "home fan cap regressed past PWM 30 — load-bearing safety clamp",
        );
        assert_eq!(fan.max_pwm, HOME_FAN_PWM_MAX);
    }

    #[test]
    fn heater_config_alias_gets_home_mode_constraints() {
        let home_mode = dcentrald_api_types::OperatingMode::from_config_str("HeAtEr").is_home();
        let c = build_bm1362_constraints(home_mode);
        let fan = c.fan_envelope.expect("fan envelope present");
        assert_eq!(c.mode, OperatingMode::ModeHome as i32);
        assert_eq!(fan.max_pwm, HOME_FAN_PWM_MAX);
    }

    #[test]
    fn standard_mode_uses_non_home_ceiling() {
        let c = build_bm1362_constraints(false);
        let fan = c.fan_envelope.expect("fan envelope present");
        assert_eq!(fan.max_pwm, NON_HOME_FAN_PWM_MAX);
    }

    #[test]
    fn freq_band_derives_from_silicon_profile_table() {
        let band = bm1362_freq_band();
        // Live-confirmed BM1362 cadence is exactly +25 MHz / step.
        assert_eq!(band.step_mhz, DEFAULT_BM1362_FREQ_STEP_MHZ);
        assert!(band.min_mhz <= 200);
        assert!(band.max_mhz >= 600);
        assert!(band.min_mhz < band.max_mhz);
    }

    #[test]
    fn mode_override_preserves_home_clamp() {
        // Operator asks for MODE_PERFORMANCE while unit is in home mode —
        // mode echoes back PERFORMANCE but the fan cap stays at 30.
        let c = build_constraints(true, OperatingMode::ModePerformance, bm1362_freq_band());
        let fan = c.fan_envelope.expect("fan envelope present");
        assert_eq!(c.mode, OperatingMode::ModePerformance as i32);
        assert_eq!(fan.max_pwm, HOME_FAN_PWM_MAX);
    }

    #[test]
    fn source_field_advertises_silicon_profiles() {
        let c = build_bm1362_constraints(false);
        assert_eq!(c.source, "silicon-profiles");
        assert_eq!(c.chip_family, "bm1362");
    }

    // ---- CE-122: per-family (platform/chip-aware) constraints --------------

    const NON_BM1362_FAMILIES: &[&str] =
        &["bm1387", "bm1397", "bm1398", "bm1366", "bm1368", "bm1370"];

    #[test]
    fn per_family_freq_band_matches_silicon_tables() {
        use dcentrald_silicon_profiles as sp;
        let cases: &[(&str, &sp::SiliconTable)] = &[
            ("bm1387", &sp::bm1387::BM1387_TABLE),
            ("bm1397", &sp::bm1397::BM1397_TABLE),
            ("bm1398", &sp::bm1398::BM1398_TABLE),
            ("bm1366", &sp::bm1366::BM1366_TABLE),
            ("bm1368", &sp::bm1368::BM1368_TABLE),
            ("bm1370", &sp::bm1370::BM1370_TABLE),
        ];
        for (family, table) in cases {
            let c = build_constraints_for_chip(family, false);
            let band = c.frequency_band.expect("freq band present");
            let want_min = table.profiles.iter().map(|p| p.freq_mhz).min().unwrap();
            let want_max = table.profiles.iter().map(|p| p.freq_mhz).max().unwrap();
            assert_eq!(band.min_mhz, want_min, "{family} min");
            assert_eq!(band.max_mhz, want_max, "{family} max");
            assert_eq!(c.chip_family, *family);
        }
    }

    #[test]
    fn non_bm1362_families_never_advertise_a_voltage_envelope() {
        for family in NON_BM1362_FAMILIES {
            let c = build_constraints_for_chip(family, false);
            assert!(
                c.voltage_envelope.is_none(),
                "{family} must not advertise a voltage envelope (14500 mV is bm1362-only)"
            );
            assert_eq!(c.source, SOURCE_FREQ_ONLY);
        }
    }

    #[test]
    fn home_fan_pwm30_cap_applies_to_every_family() {
        for family in NON_BM1362_FAMILIES.iter().chain(&["bm1362", "", "garbage"]) {
            let home = build_constraints_for_chip(family, true);
            assert_eq!(
                home.fan_envelope.unwrap().max_pwm,
                HOME_FAN_PWM_MAX,
                "{family} home fan cap"
            );
            let std = build_constraints_for_chip(family, false);
            assert_eq!(std.fan_envelope.unwrap().max_pwm, NON_HOME_FAN_PWM_MAX);
        }
    }

    #[test]
    fn unknown_or_empty_family_is_byte_identical_bm1362_default() {
        for h in [true, false] {
            assert_eq!(
                build_constraints_for_chip("", h),
                build_bm1362_constraints(h)
            );
            assert_eq!(
                build_constraints_for_chip("bm9999", h),
                build_bm1362_constraints(h)
            );
        }
    }

    #[test]
    fn chip_family_matching_is_case_insensitive() {
        assert_eq!(
            build_constraints_for_chip("BM1387", true),
            build_constraints_for_chip("bm1387", true)
        );
        assert_eq!(
            build_constraints_for_chip(" Bm1370 ", false),
            build_constraints_for_chip("bm1370", false)
        );
    }

    #[test]
    fn voltage_envelope_when_present_never_exceeds_14500() {
        for family in NON_BM1362_FAMILIES.iter().chain(&["bm1362", "", "x"]) {
            let c = build_constraints_for_chip(family, false);
            if let Some(env) = c.voltage_envelope {
                assert!(env.max_mv <= VOLTAGE_MAX_MV_AM2);
                assert_eq!(c.chip_family, "bm1362", "only bm1362 may carry an envelope");
            }
        }
    }
}
