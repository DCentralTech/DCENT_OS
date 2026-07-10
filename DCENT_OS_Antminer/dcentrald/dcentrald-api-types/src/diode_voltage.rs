//!  dvr-A — S17/S19 hashboard diode-voltage reference (HAL-free).
//!
//! Source RE evidence:
//!
//! (Bitmain AMTC official "17&19 Diode (Resistance) & Voltage Values
//! for reference" document, 109 lines).
//!
//! Hashboard repair workflow uses a Fluke 15B+ multimeter to probe per-pin
//! diode resistance and voltage. The factory-calibrated reference values
//! identify the failure class:
//! - **Dead chip**: diode values significantly outside range.
//! - **Signal chain break**: `RX/RI` or `TX/CO` out of range at chip N
//!   indicates the chain breaks at chip N.
//! - **LDO failure**: 0.8 V LDO reading wrong → voltage regulator failure.
//! - **CLK absence**: `CLK` not in 0.7-0.9 V band when powered → clock
//!   distribution problem.
//!
//! HAL-free: pure data tables + classifier function. The runtime adapter
//! (or operator-driven repair workflow in `asic-tester`) consumes this
//! catalog to render pass/fail diagnostics.
//!
//! Note: values were measured with a Fluke 15B+. Other multimeters may
//! give different readings; board batch variations also affect values.
//! Treat as reference, not absolute pass/fail.

use serde::{Deserialize, Serialize};

/// Hashboard pin labels common across S17 / S17+ / S17e / T17 / T17+ /
/// T17e / S19 / S19 Pro families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiodePin {
    /// Bus In / Bus Out — ASIC data chain signals.
    BiBo,
    /// Reset line.
    Rst,
    /// Receive / Receive In — UART/serial input.
    RxRi,
    /// Transmit / Chain Out — UART/serial output.
    TxCo,
    /// Clock signal.
    Clk,
    /// Low-dropout regulator 1.8 V I/O supply.
    Ldo1v8,
    /// Low-dropout regulator 0.8 V core supply.
    Ldo0v8,
}

/// Chip family for the diode reference tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiodeFamily {
    /// S17 / T17 / plus-family X17 reference table. S17+ / T17+ identify as
    /// BM1396 at runtime, but the retained AMTC diode worksheet groups them
    /// with the legacy X17 board-voltage family.
    S17Family,
    /// S17e / T17e — wider tolerance die revision.
    S17eFamily,
    /// S19 / S19 Pro — BM1398.
    S19Family,
}

/// One reference reading expectation. Values are nominal-with-tolerance,
/// matching the AMTC tables in the RE doc.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DiodeReadingExpectation {
    pub pin: DiodePin,
    /// Nominal diode resistance in ohms when probing with the multimeter
    /// in diode mode. `None` for pins where the AMTC doc records no
    /// diode value (the LDO pins).
    pub diode_ohms: Option<u32>,
    /// Tolerance band for `diode_ohms`, in ohms.
    pub diode_tolerance_ohms: Option<u32>,
    /// Nominal voltage in volts when powered. `None` for LDO supplies
    /// (no per-pin voltage spec in the doc, just resistance).
    pub voltage_v: Option<f32>,
    /// Tolerance band for `voltage_v`, in volts. `None` when the doc
    /// gives a range (CLK = 0.7-0.9 V) — caller uses
    /// `voltage_range_v` instead for those.
    pub voltage_tolerance_v: Option<f32>,
    /// Voltage range (when the doc gives min/max instead of nominal +
    /// tolerance). Used for CLK = 0.7-0.9 V.
    pub voltage_range_v: Option<(f32, f32)>,
}

/// Operator-facing diagnostic verdict for a measured value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiodeVerdict {
    /// Reading is within the documented tolerance band.
    Healthy,
    /// Reading is outside tolerance but within "drift" range
    /// (less than 2× the tolerance off-nominal).
    OutOfTolerance,
    /// Reading is essentially zero — pin is shorted to ground.
    Shorted,
    /// Reading is essentially the I/O rail voltage (~1.8-3.3 V on a
    /// supposed 0 V pin) — pin is open / no driver.
    Open,
    /// No reference for this (family, pin) tuple.
    NotReferenced,
}

/// S17 / T17 / S17+ / T17+ table per RE doc lines 25-52.
pub const S17_REFERENCE: &[DiodeReadingExpectation] = &[
    DiodeReadingExpectation {
        pin: DiodePin::BiBo,
        diode_ohms: Some(1200),
        diode_tolerance_ohms: Some(20),
        voltage_v: Some(0.0),
        voltage_tolerance_v: Some(0.05),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::Rst,
        diode_ohms: Some(1200),
        diode_tolerance_ohms: Some(20),
        voltage_v: Some(1.7),
        voltage_tolerance_v: Some(0.1),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::RxRi,
        diode_ohms: Some(420),
        diode_tolerance_ohms: Some(20),
        voltage_v: Some(1.7),
        voltage_tolerance_v: Some(0.1),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::TxCo,
        diode_ohms: Some(1200),
        diode_tolerance_ohms: Some(20),
        voltage_v: Some(1.7),
        voltage_tolerance_v: Some(0.1),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::Clk,
        diode_ohms: Some(1200),
        diode_tolerance_ohms: Some(20),
        voltage_v: None,
        voltage_tolerance_v: None,
        voltage_range_v: Some((0.7, 0.9)),
    },
    DiodeReadingExpectation {
        pin: DiodePin::Ldo1v8,
        diode_ohms: Some(400),
        diode_tolerance_ohms: Some(20),
        voltage_v: None,
        voltage_tolerance_v: None,
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::Ldo0v8,
        diode_ohms: Some(20),
        diode_tolerance_ohms: Some(5),
        voltage_v: None,
        voltage_tolerance_v: None,
        voltage_range_v: None,
    },
];

/// S17e / T17e table per RE doc lines 36-58 (different die revision —
/// wider tolerances).
pub const S17E_REFERENCE: &[DiodeReadingExpectation] = &[
    DiodeReadingExpectation {
        pin: DiodePin::BiBo,
        diode_ohms: Some(1015),
        diode_tolerance_ohms: Some(50),
        voltage_v: Some(0.0),
        voltage_tolerance_v: Some(0.05),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::Rst,
        diode_ohms: Some(970),
        diode_tolerance_ohms: Some(50),
        voltage_v: Some(1.7),
        voltage_tolerance_v: Some(0.1),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::RxRi,
        diode_ohms: Some(500),
        diode_tolerance_ohms: Some(50),
        voltage_v: Some(1.7),
        voltage_tolerance_v: Some(0.1),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::TxCo,
        diode_ohms: Some(1015),
        diode_tolerance_ohms: Some(50),
        voltage_v: Some(1.7),
        voltage_tolerance_v: Some(0.1),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::Clk,
        diode_ohms: Some(1015),
        diode_tolerance_ohms: Some(50),
        voltage_v: None,
        voltage_tolerance_v: None,
        voltage_range_v: Some((0.7, 0.9)),
    },
    DiodeReadingExpectation {
        pin: DiodePin::Ldo1v8,
        diode_ohms: Some(400),
        diode_tolerance_ohms: Some(50),
        voltage_v: None,
        voltage_tolerance_v: None,
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::Ldo0v8,
        diode_ohms: Some(25),
        diode_tolerance_ohms: Some(5),
        voltage_v: None,
        voltage_tolerance_v: None,
        voltage_range_v: None,
    },
];

/// S19 / S19 Pro table per RE doc lines 60-72 (BM1398 silicon).
pub const S19_REFERENCE: &[DiodeReadingExpectation] = &[
    DiodeReadingExpectation {
        pin: DiodePin::BiBo,
        diode_ohms: Some(1220),
        diode_tolerance_ohms: Some(20),
        voltage_v: Some(0.0),
        voltage_tolerance_v: Some(0.05),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::Rst,
        diode_ohms: Some(980),
        diode_tolerance_ohms: Some(20),
        voltage_v: Some(1.7),
        voltage_tolerance_v: Some(0.1),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::RxRi,
        diode_ohms: Some(390),
        diode_tolerance_ohms: Some(20),
        voltage_v: Some(1.7),
        voltage_tolerance_v: Some(0.1),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::TxCo,
        diode_ohms: Some(1220),
        diode_tolerance_ohms: Some(20),
        voltage_v: Some(1.7),
        voltage_tolerance_v: Some(0.1),
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::Clk,
        diode_ohms: Some(1220),
        diode_tolerance_ohms: Some(20),
        voltage_v: None,
        voltage_tolerance_v: None,
        voltage_range_v: Some((0.7, 0.9)),
    },
    DiodeReadingExpectation {
        pin: DiodePin::Ldo1v8,
        diode_ohms: Some(440),
        diode_tolerance_ohms: Some(20),
        voltage_v: None,
        voltage_tolerance_v: None,
        voltage_range_v: None,
    },
    DiodeReadingExpectation {
        pin: DiodePin::Ldo0v8,
        diode_ohms: Some(20),
        diode_tolerance_ohms: Some(5),
        voltage_v: None,
        voltage_tolerance_v: None,
        voltage_range_v: None,
    },
];

/// Look up the reference table for a chip family.
pub fn (family: DiodeFamily) -> &'static [DiodeReadingExpectation] {
    match family {
        DiodeFamily::S17Family => S17_REFERENCE,
        DiodeFamily::S17eFamily => S17E_REFERENCE,
        DiodeFamily::S19Family => S19_REFERENCE,
    }
}

/// Look up the expectation for a specific (family, pin) tuple.
pub fn expectation(family: DiodeFamily, pin: DiodePin) -> Option<&'static DiodeReadingExpectation> {
    (family).iter().find(|e| e.pin == pin)
}

/// Classify a measured voltage against the reference table.
///
/// Returns:
/// - `Healthy` when within the documented tolerance.
/// - `OutOfTolerance` when within 2× tolerance of nominal but not strict.
/// - `Shorted` when reading <= 0.05 V on a pin that should NOT be 0 V.
/// - `Open` when reading >= 1.5 V on a pin that should be 0 V.
/// - `NotReferenced` when the (family, pin) tuple has no voltage spec
///   (e.g. LDO pins).
pub fn classify_voltage(family: DiodeFamily, pin: DiodePin, measured_v: f32) -> DiodeVerdict {
    let exp = match expectation(family, pin) {
        Some(e) => e,
        None => return DiodeVerdict::NotReferenced,
    };
    if let Some((min_v, max_v)) = exp.voltage_range_v {
        // Range form (e.g. CLK 0.7-0.9 V).
        if measured_v >= min_v && measured_v <= max_v {
            return DiodeVerdict::Healthy;
        }
        if measured_v <= 0.05 {
            return DiodeVerdict::Shorted;
        }
        if measured_v >= 1.5 {
            return DiodeVerdict::Open;
        }
        let span = max_v - min_v;
        if measured_v >= min_v - span && measured_v <= max_v + span {
            return DiodeVerdict::OutOfTolerance;
        }
        return DiodeVerdict::OutOfTolerance;
    }
    if let (Some(nominal), Some(tol)) = (exp.voltage_v, exp.voltage_tolerance_v) {
        let diff = (measured_v - nominal).abs();
        if diff <= tol {
            return DiodeVerdict::Healthy;
        }
        // For "should be 0 V" pins (BI/BO), a high reading means the
        // pin is open (no driver biasing it back to 0).
        if nominal == 0.0 && measured_v >= 1.5 {
            return DiodeVerdict::Open;
        }
        // For "should be 1.7 V" pins (RST/RX/TX), a near-zero reading
        // means the pin is shorted to ground.
        if nominal > 1.0 && measured_v <= 0.1 {
            return DiodeVerdict::Shorted;
        }
        if diff <= tol * 2.0 {
            return DiodeVerdict::OutOfTolerance;
        }
        // Outside 2× tolerance — still OutOfTolerance, but the open/
        // shorted classifiers above would have caught the extremes.
        return DiodeVerdict::OutOfTolerance;
    }
    DiodeVerdict::NotReferenced
}

/// Classify a measured diode resistance against the reference table.
///
/// Returns:
/// - `Healthy` when within the documented `diode_tolerance_ohms`.
/// - `OutOfTolerance` when within 2× tolerance.
/// - `Shorted` when reading is essentially zero (≤ 5 Ω) on a pin that
///   should NOT be 0 Ω.
/// - `Open` when reading is essentially infinite (multimeter shows
///   "OL"; we model as `f32::INFINITY` or a large sentinel).
/// - `NotReferenced` when the family/pin tuple has no diode spec.
pub fn classify_diode_ohms(family: DiodeFamily, pin: DiodePin, measured_ohms: u32) -> DiodeVerdict {
    let exp = match expectation(family, pin) {
        Some(e) => e,
        None => return DiodeVerdict::NotReferenced,
    };
    let (nominal, tol) = match (exp.diode_ohms, exp.diode_tolerance_ohms) {
        (Some(n), Some(t)) => (n, t),
        _ => return DiodeVerdict::NotReferenced,
    };
    let diff = (measured_ohms as i64 - nominal as i64).unsigned_abs() as u32;
    if diff <= tol {
        return DiodeVerdict::Healthy;
    }
    if measured_ohms <= 5 && nominal > 50 {
        return DiodeVerdict::Shorted;
    }
    if measured_ohms > nominal.saturating_mul(10) && nominal > 0 {
        return DiodeVerdict::Open;
    }
    DiodeVerdict::OutOfTolerance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_has_seven_pin_entries() {
        for fam in [
            DiodeFamily::S17Family,
            DiodeFamily::S17eFamily,
            DiodeFamily::S19Family,
        ] {
            let table = (fam);
            assert_eq!(table.len(), 7, "{:?} should have 7 pin entries", fam);
        }
    }

    #[test]
    fn s17_bi_bo_resistance_anchor_matches_re_doc() {
        let exp = expectation(DiodeFamily::S17Family, DiodePin::BiBo).unwrap();
        assert_eq!(exp.diode_ohms, Some(1200));
        assert_eq!(exp.diode_tolerance_ohms, Some(20));
        assert_eq!(exp.voltage_v, Some(0.0));
    }

    #[test]
    fn s17e_has_wider_tolerances() {
        // Per RE doc lines 36-58: S17e/T17e is "different silicon
        // (wider tolerances)". Our table records ±50 vs ±20.
        let s17 = expectation(DiodeFamily::S17Family, DiodePin::BiBo).unwrap();
        let s17e = expectation(DiodeFamily::S17eFamily, DiodePin::BiBo).unwrap();
        assert!(s17e.diode_tolerance_ohms.unwrap() > s17.diode_tolerance_ohms.unwrap());
    }

    #[test]
    fn s19_has_higher_bi_bo_resistance() {
        // Per RE doc cross-model analysis lines 79-83.
        let s17 = expectation(DiodeFamily::S17Family, DiodePin::BiBo).unwrap();
        let s19 = expectation(DiodeFamily::S19Family, DiodePin::BiBo).unwrap();
        assert!(s19.diode_ohms.unwrap() > s17.diode_ohms.unwrap());
    }

    #[test]
    fn classify_voltage_healthy_within_tolerance() {
        // RST nominal 1.7 ± 0.1 V — measured 1.65 V is healthy.
        let v = classify_voltage(DiodeFamily::S17Family, DiodePin::Rst, 1.65);
        assert_eq!(v, DiodeVerdict::Healthy);
    }

    #[test]
    fn classify_voltage_shorted_when_high_pin_reads_zero() {
        // RST should be 1.7 V; if it reads 0.05 V, the pin is shorted.
        let v = classify_voltage(DiodeFamily::S17Family, DiodePin::Rst, 0.05);
        assert_eq!(v, DiodeVerdict::Shorted);
    }

    #[test]
    fn classify_voltage_open_when_zero_pin_reads_high() {
        // BI/BO should be 0 V; if it reads 1.8 V, pin is open.
        let v = classify_voltage(DiodeFamily::S17Family, DiodePin::BiBo, 1.8);
        assert_eq!(v, DiodeVerdict::Open);
    }

    #[test]
    fn classify_voltage_clk_range_band() {
        // CLK should be 0.7-0.9 V.
        assert_eq!(
            classify_voltage(DiodeFamily::S19Family, DiodePin::Clk, 0.8),
            DiodeVerdict::Healthy
        );
        // Below the band → OutOfTolerance.
        assert_eq!(
            classify_voltage(DiodeFamily::S19Family, DiodePin::Clk, 0.6),
            DiodeVerdict::OutOfTolerance
        );
        // Way below → Shorted.
        assert_eq!(
            classify_voltage(DiodeFamily::S19Family, DiodePin::Clk, 0.0),
            DiodeVerdict::Shorted
        );
    }

    #[test]
    fn classify_diode_ohms_healthy_at_nominal() {
        let v = classify_diode_ohms(DiodeFamily::S19Family, DiodePin::BiBo, 1220);
        assert_eq!(v, DiodeVerdict::Healthy);
        let v = classify_diode_ohms(DiodeFamily::S19Family, DiodePin::BiBo, 1230);
        assert_eq!(v, DiodeVerdict::Healthy);
    }

    #[test]
    fn classify_diode_ohms_shorted_when_near_zero() {
        let v = classify_diode_ohms(DiodeFamily::S19Family, DiodePin::BiBo, 0);
        assert_eq!(v, DiodeVerdict::Shorted);
        let v = classify_diode_ohms(DiodeFamily::S19Family, DiodePin::BiBo, 3);
        assert_eq!(v, DiodeVerdict::Shorted);
    }

    #[test]
    fn classify_diode_ohms_out_of_tolerance_at_50pct_off() {
        // S19 BI/BO nominal 1220 ± 20. 1300 is well outside but not
        // 10× off → OutOfTolerance.
        let v = classify_diode_ohms(DiodeFamily::S19Family, DiodePin::BiBo, 1300);
        assert_eq!(v, DiodeVerdict::OutOfTolerance);
    }

    #[test]
    fn classify_voltage_not_referenced_for_ldo() {
        // LDO pins have no voltage spec — return NotReferenced.
        let v = classify_voltage(DiodeFamily::S19Family, DiodePin::Ldo1v8, 1.8);
        assert_eq!(v, DiodeVerdict::NotReferenced);
    }

    #[test]
    fn diode_pin_round_trips_through_serde() {
        for pin in [
            DiodePin::BiBo,
            DiodePin::Rst,
            DiodePin::RxRi,
            DiodePin::TxCo,
            DiodePin::Clk,
            DiodePin::Ldo1v8,
            DiodePin::Ldo0v8,
        ] {
            let json = serde_json::to_string(&pin).unwrap();
            let back: DiodePin = serde_json::from_str(&json).unwrap();
            assert_eq!(pin, back);
        }
    }

    #[test]
    fn diode_verdict_round_trips_through_serde() {
        for v in [
            DiodeVerdict::Healthy,
            DiodeVerdict::OutOfTolerance,
            DiodeVerdict::Shorted,
            DiodeVerdict::Open,
            DiodeVerdict::NotReferenced,
        ] {
            let json = serde_json::to_string(&v).unwrap();
            let back: DiodeVerdict = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }
}
