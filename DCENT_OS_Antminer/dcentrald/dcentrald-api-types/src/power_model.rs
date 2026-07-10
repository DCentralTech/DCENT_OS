//!  pwr-B — V²f power estimation model (HAL-free).
//!
//! Source RE evidence:
//!
//! lines 14-49.
//!
//! The classical CMOS dynamic-power model: P = C_eff × V² × f
//! where:
//! - `C_eff` is the effective capacitance per chip (process-node dependent).
//! - `V` is the chain rail voltage (in volts) supplied to the chip.
//! - `f` is the operating frequency in Hz.
//!
//! Total wall power adds:
//! - Static per-chain power (leakage + idle subsystems).
//! - Control-board power (dashboard, UART/USB, fan controllers).
//! - Fan power (base + dynamic = quadratic in RPM/MAX_RPM).
//!
//! HAL-free: pure float math + per-family lookup. The runtime adapter
//! inside `dcentrald-autotuner` consumes this to estimate watts when no
//! PSU PMBus telemetry is available (S9 / am2 with APW121215a).
//!
//! The RE doc's calibration anchor: BM1387 @ 189 chips × 650 MHz × 9.1 V
//! → ~1180 W (live S9). Tests pin this within 5 % tolerance.

use crate::chip_init::ChipFamily;
use serde::{Deserialize, Serialize};

/// Per-family V²f power-model coefficients per RE doc lines 30-49.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PowerModel {
    /// Effective capacitance coefficient. Units: watts per
    /// (V² × MHz × chip). Anchor (RE doc): BM1387 0.000116 ×
    /// 9.1² × 650 MHz × 189 chips ≈ 1180 W.
    pub c_eff: f64,
    /// Static per-chain power in watts (leakage + chain-level idle).
    pub static_per_chain_w: f64,
    /// Control-board power in watts (dashboard + UART + USB + fan ctrl).
    pub control_board_w: f64,
    /// Per-fan idle power in watts.
    pub fan_base_w: f64,
    /// Per-fan dynamic max in watts at MAX_RPM.
    pub fan_dynamic_max_w: f64,
}

impl PowerModel {
    /// Bosminer-canonical PowerModel for a chip family per RE doc.
    pub fn for_family(family: ChipFamily) -> Self {
        match family {
            ChipFamily::Bm1387 => Self {
                c_eff: 0.000_116,
                static_per_chain_w: 45.0,
                control_board_w: 25.0,
                fan_base_w: 5.0,
                fan_dynamic_max_w: 30.0,
            },
            ChipFamily::Bm1397 => Self {
                c_eff: 0.000_048_5,
                static_per_chain_w: 80.0,
                control_board_w: 35.0,
                fan_base_w: 5.0,
                fan_dynamic_max_w: 30.0,
            },
            ChipFamily::Bm1398 => Self {
                c_eff: 0.000_041_4,
                static_per_chain_w: 80.0,
                control_board_w: 35.0,
                fan_base_w: 5.0,
                fan_dynamic_max_w: 30.0,
            },
            ChipFamily::Bm1362 => Self {
                c_eff: 0.000_028_8,
                static_per_chain_w: 80.0,
                control_board_w: 35.0,
                fan_base_w: 5.0,
                fan_dynamic_max_w: 30.0,
            },
            ChipFamily::Bm1366 => Self {
                c_eff: 0.000_027_5,
                static_per_chain_w: 80.0,
                control_board_w: 35.0,
                fan_base_w: 5.0,
                fan_dynamic_max_w: 30.0,
            },
            ChipFamily::Bm1368 => Self {
                c_eff: 0.000_017_8,
                static_per_chain_w: 80.0,
                control_board_w: 35.0,
                fan_base_w: 5.0,
                fan_dynamic_max_w: 30.0,
            },
            ChipFamily::Bm1370 => Self {
                c_eff: 0.000_008_8,
                static_per_chain_w: 80.0,
                control_board_w: 35.0,
                fan_base_w: 5.0,
                fan_dynamic_max_w: 30.0,
            },
            //  W5-A: scrypt families. C_eff placeholder derived
            // from the L3+ nameplate (504 MH/s @ 800 W, 12 cores/chip,
            // 384 MHz, 10.0 V chain rail) — not chip-rail-validated.
            // Power-model consumers SHOULD prefer the per-row preset
            // catalog over `for_family` for scrypt until .
            ChipFamily::Bm1485 => Self {
                c_eff: 0.000_180,
                static_per_chain_w: 50.0,
                control_board_w: 25.0,
                fan_base_w: 5.0,
                fan_dynamic_max_w: 30.0,
            },
            ChipFamily::Bm1489 => Self {
                c_eff: 0.000_140,
                static_per_chain_w: 60.0,
                control_board_w: 25.0,
                fan_base_w: 5.0,
                fan_dynamic_max_w: 30.0,
            },
            //  W8-A: NAMED-ONLY placeholder PowerModel. c_eff=0
            // makes `estimate_total_watts` return only the static +
            // control + fan terms (chip-dynamic term collapses to 0).
            // This is a refuse-to-mine sentinel — autotuner consumers
            // should treat `c_eff == 0` as "no validated chip-dynamic
            // model; do not run autotune". chip parameters genuinely
            // UNKNOWN per W7-A. [GAP — wave-9 live verification needed]
            ChipFamily::Bm1360 => Self {
                c_eff: 0.0,
                static_per_chain_w: 0.0,
                control_board_w: 0.0,
                fan_base_w: 0.0,
                fan_dynamic_max_w: 0.0,
            },
            ChipFamily::Bm1491 => Self {
                c_eff: 0.0,
                static_per_chain_w: 0.0,
                control_board_w: 0.0,
                fan_base_w: 0.0,
                fan_dynamic_max_w: 0.0,
            },
        }
    }
}

/// One chain's operating point for power estimation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChainOp {
    /// Chain rail voltage in millivolts (caller converts from float V).
    pub voltage_mv: u32,
    /// Operating frequency in MHz.
    pub frequency_mhz: u32,
    /// Number of chips on this chain.
    pub chip_count: u32,
}

/// Aggregate fan operating point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FanOp {
    /// Number of fans on the unit.
    pub count: u32,
    /// Current fan RPM.
    pub rpm: u32,
    /// Max RPM (model-specific, typically ~6000).
    pub max_rpm: u32,
}

/// Estimate total wall power in watts for a miner running at the given
/// operating point.
pub fn estimate_total_watts(model: &PowerModel, chains: &[ChainOp], fans: FanOp) -> f64 {
    let mut total = model.control_board_w;
    for c in chains {
        let v = (c.voltage_mv as f64) / 1000.0;
        // C_eff coefficients in the RE doc are calibrated with f in MHz
        // (not Hz). Anchor: BM1387 0.000116 × 9.1² × 650 × 189 ≈ 1180 W.
        let f_mhz = c.frequency_mhz as f64;
        let p_chip = model.c_eff * v * v * f_mhz;
        let p_chain = p_chip * (c.chip_count as f64) + model.static_per_chain_w;
        total += p_chain;
    }
    if fans.count > 0 && fans.max_rpm > 0 {
        let ratio = (fans.rpm as f64) / (fans.max_rpm as f64);
        let dynamic = ratio * ratio * model.fan_dynamic_max_w;
        let per_fan = model.fan_base_w + dynamic;
        total += per_fan * (fans.count as f64);
    }
    total
}

// ---------------------------------------------------------------------
// TunerDriver observe-only shadow input helper (HAL-free).
// ---------------------------------------------------------------------

/// Map an ASIC chip-id (e.g. `0x1362`) to its [`ChipFamily`] for the
/// power model. Pure lookup, no I/O. Returns `None` for an unknown id so
/// the caller can decide the fallback (the TunerDriver shadow falls back
/// to the BM1387 model, the most conservative/validated coefficient).
///
/// This is the inverse of [`ChipFamily::chip_id_byte_pair`] and keeps the
/// shadow's chip→family resolution pure + host-testable instead of pulling
/// the HAL-coupled `dcentrald-asic` chip registry into the no-HAL crate.
pub fn chip_family_from_chip_id(chip_id: u16) -> Option<ChipFamily> {
    Some(match chip_id {
        0x1387 => ChipFamily::Bm1387,
        0x1397 => ChipFamily::Bm1397,
        0x1398 => ChipFamily::Bm1398,
        0x1362 => ChipFamily::Bm1362,
        0x1366 => ChipFamily::Bm1366,
        0x1368 => ChipFamily::Bm1368,
        0x1370 => ChipFamily::Bm1370,
        0x1485 => ChipFamily::Bm1485,
        0x1489 => ChipFamily::Bm1489,
        0x1360 => ChipFamily::Bm1360,
        0x1491 => ChipFamily::Bm1491,
        _ => return None,
    })
}

/// The four observe-only inputs the daemon-side `TunerDriver` decision
/// method (`step(TelemetrySample) -> TunerOutcome`, in the `dcentrald`
/// crate) consumes, derived purely from existing live miner state. This
/// struct is the HAL-free, host-testable bridge: the daemon reads the live
/// `MinerState` watch channel (per-chain voltage/freq/chip-count, total
/// hashrate, fan PWM) and feeds the raw primitives to
/// [`TunerShadowTelemetry::from_live_state`], then copies these four
/// fields verbatim into the `TelemetrySample` it hands the shadow
/// `TunerDriver`.
///
/// It is OBSERVE-ONLY: nothing here writes anything. `actual_watts` is the
/// existing no-HAL V²f estimate ([`estimate_total_watts`]) — the same model
/// the live autotuner uses when no PSU PMBus telemetry is available — so the
/// shadow's "what would the tuner decide" output is computed from the
/// canonical power model, not a fabricated number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TunerShadowTelemetry {
    /// Estimated board/chip power right now (Watts), from the V²f model
    /// over the live per-chain operating point. Feeds
    /// `TelemetrySample::actual_watts`.
    pub actual_watts: f64,
    /// Current full-miner hashrate (TH/s). Feeds
    /// `TelemetrySample::hashrate_ths`.
    pub hashrate_ths: f64,
    /// Representative chain voltage (mV) — the max live per-chain rail, the
    /// safe choice for the shadow's universal voltage clamp check. Feeds
    /// `TelemetrySample::voltage_mv`. 0 when no chains are present.
    pub voltage_mv: u16,
    /// Current fan PWM percent (0–100). Feeds `TelemetrySample::fan_pwm`.
    pub fan_pwm: u8,
}

impl TunerShadowTelemetry {
    /// Build the shadow inputs from raw live-state primitives.
    ///
    /// * `chip_id` — the effective chip family id (e.g. `0x1362`). Unknown
    ///   ids fall back to the BM1387 power model (most conservative).
    /// * `hashrate_ghs` — total miner hashrate in GH/s (converted to TH/s).
    /// * `fan_pwm` — fan PWM percent (0–100), copied through verbatim.
    /// * `fans` — `(fan_count, fan_rpm, fan_max_rpm)` for the fan-power term.
    /// * `chains` — per-chain `(voltage_mv, frequency_mhz, chip_count)`
    ///   tuples straight from the live per-chain status.
    ///
    /// Pure: no clock, no I/O, no hardware. `voltage_mv` is the MAX live
    /// rail (so the shadow's clamp check sees the worst case). When `chains`
    /// is empty, `actual_watts` collapses to the static/control/fan terms
    /// and `voltage_mv` is 0 — both honest "not mining / unknown" readings
    /// for an observe-only log.
    pub fn from_live_state(
        chip_id: u16,
        hashrate_ghs: f64,
        fan_pwm: u8,
        fans: (u32, u32, u32),
        chains: &[(u16, u16, u32)],
    ) -> Self {
        let family = chip_family_from_chip_id(chip_id).unwrap_or(ChipFamily::Bm1387);
        let model = PowerModel::for_family(family);

        let chain_ops: Vec<ChainOp> = chains
            .iter()
            .map(|&(voltage_mv, frequency_mhz, chip_count)| ChainOp {
                voltage_mv: voltage_mv as u32,
                frequency_mhz: frequency_mhz as u32,
                chip_count,
            })
            .collect();

        let (fan_count, fan_rpm, fan_max_rpm) = fans;
        let fan_op = FanOp {
            count: fan_count,
            rpm: fan_rpm,
            max_rpm: fan_max_rpm,
        };

        let actual_watts = estimate_total_watts(&model, &chain_ops, fan_op);

        // Representative rail = the highest live per-chain voltage. The
        // shadow's TunerDriver applies a universal `voltage <= 14_500 mV`
        // clamp; feeding the MAX is the safe (worst-case) reading.
        let voltage_mv = chains.iter().map(|&(v, _, _)| v).max().unwrap_or(0);

        Self {
            actual_watts,
            // GH/s → TH/s. This bare `/ 1000.0` is the exact 1000x GHS↔THS
            // hazard that `dcentrald_common::units::ghs_to_ths` (gap-swarm G62)
            // guards; api-types does not depend on dcentrald-common so the
            // helper is unreachable here — keep this the ONLY hashrate-unit
            // conversion in this fn and never assign a GH/s value straight into
            // a `*_ths` field. Pinned by tuner_shadow_telemetry_matches_estimate_total_watts.
            hashrate_ths: hashrate_ghs / 1000.0,
            voltage_mv,
            fan_pwm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s9_chains_at_650mhz() -> Vec<ChainOp> {
        // S9: 3 chains × 63 chips = 189 chips total.
        (0..3)
            .map(|_| ChainOp {
                voltage_mv: 9_100,
                frequency_mhz: 650,
                chip_count: 63,
            })
            .collect()
    }

    fn s9_fans() -> FanOp {
        FanOp {
            count: 2,
            rpm: 3000,
            max_rpm: 6000,
        }
    }

    #[test]
    fn re_doc_bm1387_chip_dynamic_anchor_within_5pct() {
        // RE doc anchor (line 35): "BM1387 16 nm 0.000116 VERIFIED —
        // 189 chips × 650 MHz × 9.1 V → 1180 W (live S9)." This anchor
        // covers the chip-dynamic portion only (C_eff × V² × f × chips),
        // NOT the static + control + fan additions. We verify that
        // portion separately by feeding zero-static / no-fan chains:
        // synthesize a PowerModel with static=0/control=0 and run the
        // same estimator.
        let mut model = PowerModel::for_family(ChipFamily::Bm1387);
        model.static_per_chain_w = 0.0;
        model.control_board_w = 0.0;
        let chains = s9_chains_at_650mhz();
        let no_fans = FanOp {
            count: 0,
            rpm: 0,
            max_rpm: 6000,
        };
        let p = estimate_total_watts(&model, &chains, no_fans);
        // Target 1180; allow 5 % tolerance.
        let lo = 1180.0 * 0.95;
        let hi = 1180.0 * 1.05;
        assert!(
            p >= lo && p <= hi,
            "chip-dynamic estimate {} W; expected [{}, {}]",
            p,
            lo,
            hi
        );
    }

    #[test]
    fn s9_full_wall_power_within_8pct_of_nameplate() {
        // S9 nameplate: ~1320 W full wall @ 13.5 TH/s. The model adds
        // static (3 × 45 W) + control (25 W) + fan power on top of the
        // chip-dynamic 1180 W. With realistic ~half-RPM fans, the total
        // should land near nameplate within ~8 %.
        let model = PowerModel::for_family(ChipFamily::Bm1387);
        let chains = s9_chains_at_650mhz();
        let p = estimate_total_watts(&model, &chains, s9_fans());
        let lo = 1320.0 * 0.92;
        let hi = 1320.0 * 1.08;
        assert!(
            p >= lo && p <= hi,
            "full-wall estimate {} W; expected [{}, {}]",
            p,
            lo,
            hi
        );
    }

    #[test]
    fn lower_voltage_yields_lower_estimate() {
        let model = PowerModel::for_family(ChipFamily::Bm1387);
        let chains_high = vec![ChainOp {
            voltage_mv: 9_400,
            frequency_mhz: 600,
            chip_count: 63,
        }];
        let chains_low = vec![ChainOp {
            voltage_mv: 8_900,
            frequency_mhz: 600,
            chip_count: 63,
        }];
        let p_high = estimate_total_watts(&model, &chains_high, s9_fans());
        let p_low = estimate_total_watts(&model, &chains_low, s9_fans());
        assert!(p_low < p_high);
    }

    #[test]
    fn lower_frequency_yields_lower_estimate() {
        let model = PowerModel::for_family(ChipFamily::Bm1387);
        let high = vec![ChainOp {
            voltage_mv: 9_100,
            frequency_mhz: 650,
            chip_count: 63,
        }];
        let low = vec![ChainOp {
            voltage_mv: 9_100,
            frequency_mhz: 450,
            chip_count: 63,
        }];
        let p_high = estimate_total_watts(&model, &high, s9_fans());
        let p_low = estimate_total_watts(&model, &low, s9_fans());
        assert!(p_low < p_high);
    }

    #[test]
    fn fan_dynamic_term_is_quadratic_in_rpm_ratio() {
        let model = PowerModel::for_family(ChipFamily::Bm1387);
        let chains = s9_chains_at_650mhz();
        let half_rpm = FanOp {
            count: 2,
            rpm: 3000,
            max_rpm: 6000,
        };
        let full_rpm = FanOp {
            count: 2,
            rpm: 6000,
            max_rpm: 6000,
        };
        let p_half = estimate_total_watts(&model, &chains, half_rpm);
        let p_full = estimate_total_watts(&model, &chains, full_rpm);
        // Fan power at full RPM is ~4× the dynamic term at half RPM,
        // not 2× — that's the quadratic invariant we're pinning.
        let fan_diff = p_full - p_half;
        // 2 fans × (full quadratic 30 - half quadratic 7.5) = 45 W diff.
        assert!(
            (fan_diff - 45.0).abs() < 0.1,
            "fan dynamic diff was {} W (expected ~45)",
            fan_diff
        );
    }

    #[test]
    fn control_board_power_is_fixed_addition() {
        let model = PowerModel::for_family(ChipFamily::Bm1387);
        let no_chains = Vec::<ChainOp>::new();
        let no_fans = FanOp {
            count: 0,
            rpm: 0,
            max_rpm: 6000,
        };
        let p = estimate_total_watts(&model, &no_chains, no_fans);
        assert!((p - model.control_board_w).abs() < 0.01);
    }

    #[test]
    fn zero_chips_yields_static_per_chain_only() {
        let model = PowerModel::for_family(ChipFamily::Bm1387);
        let chains = vec![ChainOp {
            voltage_mv: 9_100,
            frequency_mhz: 600,
            chip_count: 0,
        }];
        let no_fans = FanOp {
            count: 0,
            rpm: 0,
            max_rpm: 6000,
        };
        let p = estimate_total_watts(&model, &chains, no_fans);
        // control_board_w + static_per_chain_w (1 chain).
        assert!((p - (model.control_board_w + model.static_per_chain_w)).abs() < 0.01);
    }

    #[test]
    fn zero_fan_count_skips_fan_power() {
        let model = PowerModel::for_family(ChipFamily::Bm1387);
        let chains = vec![ChainOp {
            voltage_mv: 9_100,
            frequency_mhz: 650,
            chip_count: 63,
        }];
        let zero_fans = FanOp {
            count: 0,
            rpm: 0,
            max_rpm: 6000,
        };
        let with_fans = FanOp {
            count: 2,
            rpm: 3000,
            max_rpm: 6000,
        };
        let p_zero = estimate_total_watts(&model, &chains, zero_fans);
        let p_with = estimate_total_watts(&model, &chains, with_fans);
        assert!(p_with > p_zero);
    }

    #[test]
    fn per_family_c_eff_decreases_with_smaller_process_node() {
        // RE doc invariant: smaller process → smaller C_eff.
        // 16 nm > 7 nm > 5 nm > 5 nm (S21) > 3 nm.
        let bm1387 = PowerModel::for_family(ChipFamily::Bm1387).c_eff; // 16 nm
        let bm1397 = PowerModel::for_family(ChipFamily::Bm1397).c_eff; // 7 nm
        let bm1362 = PowerModel::for_family(ChipFamily::Bm1362).c_eff; // 5 nm
        let bm1370 = PowerModel::for_family(ChipFamily::Bm1370).c_eff; // 3 nm
        assert!(bm1387 > bm1397);
        assert!(bm1397 > bm1362);
        assert!(bm1362 > bm1370);
    }

    #[test]
    fn power_model_round_trips_through_serde() {
        let m = PowerModel::for_family(ChipFamily::Bm1362);
        let json = serde_json::to_string(&m).unwrap();
        let back: PowerModel = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn chain_op_and_fan_op_round_trip_through_serde() {
        let c = ChainOp {
            voltage_mv: 9_100,
            frequency_mhz: 650,
            chip_count: 63,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: ChainOp = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
        let f = FanOp {
            count: 2,
            rpm: 3000,
            max_rpm: 6000,
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: FanOp = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    // -----------------------------------------------------------------
    // TunerDriver observe-only shadow input helper
    // -----------------------------------------------------------------

    #[test]
    fn chip_family_from_chip_id_round_trips_every_family() {
        // Every family the model knows must map from its canonical chip id.
        for family in [
            ChipFamily::Bm1387,
            ChipFamily::Bm1397,
            ChipFamily::Bm1398,
            ChipFamily::Bm1362,
            ChipFamily::Bm1366,
            ChipFamily::Bm1368,
            ChipFamily::Bm1370,
            ChipFamily::Bm1485,
            ChipFamily::Bm1489,
            ChipFamily::Bm1360,
            ChipFamily::Bm1491,
        ] {
            let [hi, lo] = family.chip_id_byte_pair();
            let chip_id = ((hi as u16) << 8) | lo as u16;
            assert_eq!(
                chip_family_from_chip_id(chip_id),
                Some(family),
                "chip id 0x{:04X} must map back to {:?}",
                chip_id,
                family
            );
        }
        // Unknown id → None (caller falls back to BM1387 model).
        assert_eq!(chip_family_from_chip_id(0x0000), None);
        assert_eq!(chip_family_from_chip_id(0xFFFF), None);
    }

    #[test]
    fn tuner_shadow_telemetry_matches_estimate_total_watts() {
        // The shadow's actual_watts MUST equal the canonical V²f estimate
        // for the same operating point — it is not a separate fabrication.
        // BM1362 (0x1362), 2 chains × 76 chips @ 525 MHz, 13_700 mV, no fans.
        let chains = [(13_700u16, 525u16, 76u32), (13_700u16, 525u16, 76u32)];
        let t = TunerShadowTelemetry::from_live_state(
            0x1362,
            5_000.0, // 5000 GH/s = 5 TH/s
            25,
            (0, 0, 6000),
            &chains,
        );

        let model = PowerModel::for_family(ChipFamily::Bm1362);
        let expected_watts = estimate_total_watts(
            &model,
            &[
                ChainOp {
                    voltage_mv: 13_700,
                    frequency_mhz: 525,
                    chip_count: 76,
                },
                ChainOp {
                    voltage_mv: 13_700,
                    frequency_mhz: 525,
                    chip_count: 76,
                },
            ],
            FanOp {
                count: 0,
                rpm: 0,
                max_rpm: 6000,
            },
        );

        assert!((t.actual_watts - expected_watts).abs() < 1e-6);
        assert!((t.hashrate_ths - 5.0).abs() < 1e-9);
        assert_eq!(t.voltage_mv, 13_700);
        assert_eq!(t.fan_pwm, 25);
        // Some power is estimated (chains present + voltage > 0).
        assert!(t.actual_watts > 0.0);
    }

    #[test]
    fn tuner_shadow_telemetry_representative_voltage_is_max_chain() {
        // Mixed rails: representative voltage is the worst-case MAX so the
        // shadow's universal voltage clamp sees the highest live rail.
        let chains = [(13_200u16, 500u16, 60u32), (13_900u16, 500u16, 60u32)];
        let t = TunerShadowTelemetry::from_live_state(0x1362, 0.0, 30, (4, 3000, 6000), &chains);
        assert_eq!(t.voltage_mv, 13_900);
    }

    #[test]
    fn tuner_shadow_telemetry_empty_chains_is_honest_not_mining() {
        // No chains → voltage 0, hashrate 0, watts collapse to the
        // static/control/fan terms only (no chip-dynamic term). All honest
        // "not mining / unknown" readings for an observe-only log.
        let t = TunerShadowTelemetry::from_live_state(0x1387, 0.0, 10, (0, 0, 6000), &[]);
        assert_eq!(t.voltage_mv, 0);
        assert!((t.hashrate_ths - 0.0).abs() < 1e-9);
        assert_eq!(t.fan_pwm, 10);
        // control_board_w only (no chains, no fans).
        let model = PowerModel::for_family(ChipFamily::Bm1387);
        assert!((t.actual_watts - model.control_board_w).abs() < 0.01);
    }

    #[test]
    fn tuner_shadow_telemetry_unknown_chip_falls_back_to_bm1387() {
        // Unknown chip id → BM1387 model (the conservative fallback), same
        // numbers as if BM1387 had been passed explicitly.
        let chains = [(9_100u16, 650u16, 63u32)];
        let unknown =
            TunerShadowTelemetry::from_live_state(0xDEAD, 13_500.0, 20, (2, 3000, 6000), &chains);
        let bm1387 =
            TunerShadowTelemetry::from_live_state(0x1387, 13_500.0, 20, (2, 3000, 6000), &chains);
        assert!((unknown.actual_watts - bm1387.actual_watts).abs() < 1e-9);
    }
}
